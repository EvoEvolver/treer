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

pub(crate) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn normalize_email(email: &str) -> Result<String, AuthFailure> {
    let email = email.trim().to_ascii_lowercase();
    let valid = email.len() <= 254
        && !email
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && local.len() <= 64
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if !valid {
        return Err(AuthFailure::bad_request(
            "invalid_email",
            "enter a valid email address",
        ));
    }
    Ok(email)
}

pub(crate) fn validate_preferred_name(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return Err(AuthFailure::bad_request(
            "invalid_preferred_name",
            "preferred name must contain 1-80 visible characters",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_resource_name(value: &str, resource: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty() || value.len() > 80 || value.chars().any(|character| character.is_control())
    {
        return Err(AuthFailure::bad_request(
            "invalid_name",
            &format!("{resource} name must be 1-80 printable characters"),
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_workspace_role(role: &str) -> Result<(), AuthFailure> {
    if matches!(role, "owner" | "member") {
        Ok(())
    } else {
        Err(AuthFailure::bad_request(
            "invalid_workspace_role",
            "workspace role must be owner or member",
        ))
    }
}

pub(crate) fn validate_installation_id(value: &str) -> Result<String, AuthFailure> {
    if value.len() != 36
        || !value.starts_with("mid_")
        || !value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AuthFailure::bad_request(
            "invalid_machine_identity",
            "machine installation identity is invalid",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

pub(crate) fn validate_machine_server_id(value: &str) -> Result<String, AuthFailure> {
    if value.len() != 36
        || !value.starts_with("srv_")
        || !value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AuthFailure::bad_request(
            "invalid_machine_identity",
            "installed machine ID is invalid",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

pub(crate) fn workspace_from_row(row: sqlx::postgres::PgRow) -> Result<WorkspaceInfo, AuthFailure> {
    let created_at: String = row.get("created_at");
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            tracing::error!(%error, "invalid workspace timestamp in database");
            AuthFailure::internal(
                "database_error",
                "workspace timestamp is invalid".to_string(),
            )
        })?;
    Ok(WorkspaceInfo {
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        created_at,
    })
}

pub(crate) fn agent_launch_profile_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<AgentLaunchProfile, AuthFailure> {
    let args = serde_json::from_value::<Vec<String>>(row.get("args")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("agent launch profile has invalid args: {error}"),
        )
    })?;
    Ok(AgentLaunchProfile {
        profile_id: row.get("profile_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        description: row.get("description"),
        cwd: row.get("cwd"),
        command: row.get("command"),
        args,
        created_at: parse_database_timestamp(&row, "created_at", "agent launch profile")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "agent launch profile")?,
        updated_by: row.get("updated_by"),
    })
}

pub(crate) fn app_deployment_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<AppDeployment, AuthFailure> {
    let args = serde_json::from_value::<Vec<String>>(row.get("args")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("App deployment has invalid args: {error}"),
        )
    })?;
    let port = u16::try_from(row.get::<i64, _>("port")).map_err(|_| {
        AuthFailure::internal(
            "database_error",
            "App deployment has invalid port".to_string(),
        )
    })?;
    let restart_count = u64::try_from(row.get::<i64, _>("restart_count")).map_err(|_| {
        AuthFailure::internal(
            "database_error",
            "App deployment has invalid restart count".to_string(),
        )
    })?;
    let desired_state = match row.get::<String, _>("desired_state").as_str() {
        "running" => AppDesiredState::Running,
        "stopped" => AppDesiredState::Stopped,
        value => {
            return Err(AuthFailure::internal(
                "database_error",
                format!("App deployment has invalid desired state {value}"),
            ))
        }
    };
    Ok(AppDeployment {
        app_id: row.get("app_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        server_id: row.get("server_id"),
        command: row.get("command"),
        args,
        cwd: row.get("cwd"),
        port,
        hostname: row.get("hostname"),
        service_id: row.get("service_id"),
        public_url: None,
        access: None,
        desired_state,
        runtime_agent_id: row.get("runtime_agent_id"),
        restart_count,
        status: AppDeploymentStatus::Pending,
        pid: None,
        exit_code: None,
        last_error: row.get("last_error"),
        created_at: parse_database_timestamp(&row, "created_at", "App deployment")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "App deployment")?,
        updated_by: row.get("updated_by"),
    })
}

pub(crate) const fn app_desired_state_str(state: AppDesiredState) -> &'static str {
    match state {
        AppDesiredState::Running => "running",
        AppDesiredState::Stopped => "stopped",
    }
}

pub(crate) fn app_deployment_write_error(error: sqlx::Error) -> AuthFailure {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        AuthFailure::conflict(
            "app_conflict",
            "App name, service name, or virtual hostname already exists",
        )
    } else {
        AuthFailure::database(error)
    }
}

pub(crate) fn validate_launch_profile_description(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.chars().count() > MAX_LAUNCH_PROFILE_DESCRIPTION_CHARS
        || value.chars().any(|character| character == '\0')
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile description must be at most 1000 characters and contain no NUL bytes",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_launch_profile_cwd(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    let value = if value.is_empty() { "." } else { value };
    if value.len() > MAX_LAUNCH_PROFILE_CWD_BYTES || value.contains('\0') {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile working directory is too long or contains a NUL byte",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_launch_profile_command(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_LAUNCH_PROFILE_COMMAND_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile command must be 1-4096 printable characters",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_launch_profile_args(args: Vec<String>) -> Result<Vec<String>, AuthFailure> {
    let total_bytes = args.iter().map(String::len).sum::<usize>();
    if args.len() > MAX_LAUNCH_PROFILE_ARGS
        || total_bytes > MAX_LAUNCH_PROFILE_ARGS_BYTES
        || args
            .iter()
            .any(|arg| arg.len() > MAX_LAUNCH_PROFILE_ARG_BYTES || arg.contains('\0'))
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile args exceed the count or size limit or contain a NUL byte",
        ));
    }
    Ok(args)
}

pub(crate) fn launch_profile_write_error(error: sqlx::Error) -> AuthFailure {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        AuthFailure::conflict(
            "launch_profile_exists",
            "a launch profile with this name already exists",
        )
    } else {
        AuthFailure::database(error)
    }
}

pub(crate) async fn insert_default_agent_launch_profiles(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: &str,
    user_id: &str,
    now: &chrono::DateTime<Utc>,
) -> Result<(), AuthFailure> {
    let timestamp = now.to_rfc3339();
    for (name, description, command) in DEFAULT_AGENT_LAUNCH_PROFILES {
        sqlx::query(
            "INSERT INTO agent_launch_profiles(\
             profile_id, workspace_id, name, description, cwd, command, args, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, '.', $5, $6, $7, $8, $7, $8)",
        )
        .bind(format!("alp_{}", Uuid::new_v4().simple()))
        .bind(workspace_id)
        .bind(name)
        .bind(description)
        .bind(command)
        .bind(json!([]))
        .bind(&timestamp)
        .bind(user_id)
        .execute(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
    }
    Ok(())
}

pub(crate) async fn insert_launch_profile_audit(
    transaction: &mut Transaction<'_, Postgres>,
    profile: &AgentLaunchProfile,
    actor: ProfileMutationActor<'_>,
    action: &str,
) -> Result<(), AuthFailure> {
    let organization_id = sqlx::query_scalar::<_, String>(
        "SELECT organization_id FROM workspaces WHERE workspace_id = $1",
    )
    .bind(&profile.workspace_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(AuthFailure::database)?;
    audit::insert(
        transaction,
        NewAuditEvent {
            organization_id: &organization_id,
            workspace_id: Some(&profile.workspace_id),
            actor_kind: actor.kind,
            actor_id: actor.id,
            source: "api",
            action,
            resource_kind: "agent_launch_profile",
            resource_id: &profile.profile_id,
            resource_name: Some(&profile.name),
            payload: json!({}),
        },
    )
    .await
    .map_err(AuthFailure::database)
}

pub(crate) fn machine_service_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<MachineService, AuthFailure> {
    let target_port = u16::try_from(row.get::<i64, _>("target_port")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("machine service has invalid target_port: {error}"),
        )
    })?;
    if target_port == 0 {
        return Err(AuthFailure::internal(
            "database_error",
            "machine service target_port is zero".to_string(),
        ));
    }
    let protocol = match row.get::<String, _>("protocol").as_str() {
        "udp" => MachineServiceProtocol::Udp,
        "tcp" => MachineServiceProtocol::Tcp,
        "http" => MachineServiceProtocol::Http,
        value => {
            return Err(AuthFailure::internal(
                "database_error",
                format!("machine service has invalid protocol {value}"),
            ))
        }
    };
    Ok(MachineService {
        service_id: row.get("service_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        server_id: row.get("server_id"),
        target_agent_id: row.get("target_agent_id"),
        target_host: row.get("target_host"),
        target_port,
        protocol,
        created_at: parse_database_timestamp(&row, "created_at", "machine service")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "machine service")?,
        updated_by: row.get("updated_by"),
    })
}

pub(crate) fn parse_database_timestamp(
    row: &sqlx::postgres::PgRow,
    column: &str,
    resource: &str,
) -> Result<chrono::DateTime<Utc>, AuthFailure> {
    row.get::<String, _>(column).parse().map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("{resource} has invalid {column}: {error}"),
        )
    })
}

pub(crate) fn validate_service_target_host(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 253
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(AuthFailure::bad_request(
            "invalid_service",
            "target_host must be a non-empty hostname or address",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_service_target_agent_id(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 255
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(AuthFailure::bad_request(
            "invalid_service",
            "target_agent_id must be a non-empty Agent identifier",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn validate_agent_service_target_host(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if !matches!(value, "127.0.0.1" | "localhost" | "::1") {
        return Err(AuthFailure::bad_request(
            "invalid_agent_service_target",
            "Agent services may only target the Agent loopback interface",
        ));
    }
    Ok("127.0.0.1".to_string())
}

pub(crate) const fn machine_service_protocol_str(protocol: MachineServiceProtocol) -> &'static str {
    match protocol {
        MachineServiceProtocol::Udp => "udp",
        MachineServiceProtocol::Tcp => "tcp",
        MachineServiceProtocol::Http => "http",
    }
}

pub(crate) const fn service_ingress_access_str(access: ServiceIngressAccess) -> &'static str {
    match access {
        ServiceIngressAccess::Public => "public",
        ServiceIngressAccess::Workspace => "workspace",
    }
}

pub(crate) fn normalize_ingress_slug(value: &str) -> Result<String, AuthFailure> {
    let mut slug = String::new();
    let mut separator = false;
    for byte in value.trim().bytes() {
        if byte.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() && slug.len() < 40 {
                slug.push('-');
            }
            if slug.len() < 40 {
                slug.push(byte.to_ascii_lowercase() as char);
            }
            separator = false;
        } else {
            separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_slug",
            "ingress slug must contain an ASCII letter or number",
        ));
    }
    Ok(slug)
}

pub(crate) fn managed_app_ingress_hostname(
    app_name: &str,
    app_id: &str,
    base_domain: &str,
) -> Result<String, AuthFailure> {
    let slug = normalize_ingress_slug(app_name).unwrap_or_else(|_| "app".to_string());
    let suffix = app_id
        .strip_prefix("app_")
        .unwrap_or(app_id)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase();
    let hostname = format!("{slug}-{suffix}.{base_domain}");
    if suffix.is_empty() || hostname.len() > 253 {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_slug",
            "generated App ingress hostname is invalid",
        ));
    }
    Ok(hostname)
}

pub(crate) fn validate_ingress_return_path(value: &str) -> Result<String, AuthFailure> {
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.len() > 4096
        || value.chars().any(char::is_control)
    {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_return_path",
            "ingress return path must be a local absolute path",
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn resolved_service_ingress_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<ResolvedServiceIngress, AuthFailure> {
    let ingress = ServiceIngress {
        ingress_id: row.get("ingress_id"),
        workspace_id: row.get("workspace_id"),
        service_id: row.get("service_id"),
        hostname: row.get("hostname"),
        access: match row.get::<String, _>("access").as_str() {
            "public" => ServiceIngressAccess::Public,
            "workspace" => ServiceIngressAccess::Workspace,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("service ingress has invalid access mode {value}"),
                ))
            }
        },
        enabled: row.get("enabled"),
        created_at: parse_database_timestamp(&row, "created_at", "service ingress")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "service ingress")?,
        updated_by: row.get("updated_by"),
    };
    let target_port = u16::try_from(row.get::<i64, _>("target_port")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("service ingress target has invalid port: {error}"),
        )
    })?;
    let service = MachineService {
        service_id: ingress.service_id.clone(),
        workspace_id: ingress.workspace_id.clone(),
        name: row.get("service_name"),
        server_id: row.get("server_id"),
        target_agent_id: row.get("target_agent_id"),
        target_host: row.get("target_host"),
        target_port,
        protocol: match row.get::<String, _>("service_protocol").as_str() {
            "udp" => MachineServiceProtocol::Udp,
            "tcp" => MachineServiceProtocol::Tcp,
            "http" => MachineServiceProtocol::Http,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("service ingress target has invalid protocol {value}"),
                ))
            }
        },
        created_at: parse_database_timestamp(&row, "service_created_at", "service ingress target")?,
        created_by: row.get("service_created_by"),
        updated_at: parse_database_timestamp(&row, "service_updated_at", "service ingress target")?,
        updated_by: row.get("service_updated_by"),
    };
    Ok(ResolvedServiceIngress { ingress, service })
}

pub(crate) fn normalize_virtual_hostname(value: &str) -> Result<String, AuthFailure> {
    let hostname = value.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels_valid = !hostname.is_empty()
        && hostname.len() <= 253
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if !labels_valid {
        return Err(AuthFailure::bad_request(
            "invalid_virtual_hostname",
            "hostname must contain valid DNS labels",
        ));
    }
    Ok(hostname)
}

pub(crate) fn virtual_network_host_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<VirtualNetworkHost, AuthFailure> {
    let created_at = row
        .get::<String, _>("created_at")
        .parse()
        .map_err(|error| {
            AuthFailure::internal(
                "database_error",
                format!("virtual network host has invalid created_at: {error}"),
            )
        })?;
    let target_port = row
        .get::<Option<i64>, _>("target_port")
        .map(u16::try_from)
        .transpose()
        .map_err(|_| {
            AuthFailure::internal(
                "database_error",
                "virtual network host has invalid target_port".to_string(),
            )
        })?;
    Ok(VirtualNetworkHost {
        workspace_id: row.get("workspace_id"),
        hostname: row.get("hostname"),
        service_id: row.get("service_id"),
        service_protocol: match row.get::<String, _>("service_protocol").as_str() {
            "udp" => MachineServiceProtocol::Udp,
            "tcp" => MachineServiceProtocol::Tcp,
            "http" => MachineServiceProtocol::Http,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("virtual network host has invalid service protocol {value}"),
                ))
            }
        },
        destination_server_id: row.get("destination_server_id"),
        destination_agent_id: row.get("destination_agent_id"),
        target_host: row.get("target_host"),
        target_port,
        created_at,
        created_by: row.get("created_by"),
    })
}

pub(crate) fn hash_password(password: &str) -> Result<String, AuthFailure> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|error| AuthFailure::internal("password_hash_error", error.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| AuthFailure::internal("password_hash_error", error.to_string()))
}

pub(crate) fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}
