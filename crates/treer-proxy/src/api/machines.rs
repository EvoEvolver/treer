use super::*;

pub(super) async fn agent_issue_identity_token(
    State(state): State<AppState>,
    Extension(identity_api): Extension<WorkloadIdentityApi>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<WorkloadIdentityTokenRequest>,
) -> Result<Response, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = identity_api
        .auth
        .resolve_machine_service(&workspace_id, request.audience.trim())
        .await?;
    identity_api
        .policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject.clone(),
            ACTION_IDENTITY_TOKEN_ISSUE,
            machine_service_policy_resource(
                &service.service_id,
                &service.name,
                &service.server_id,
                service.target_agent_id.as_deref(),
                &service.target_host,
                service.target_port,
            ),
        ))
        .await?;
    let PolicySubject::Agent {
        server_id,
        agent_id,
    } = subject
    else {
        return Err(ApiFailure::internal(
            "identity_subject_error",
            "identity token subject was not an agent",
        ));
    };
    let token = identity_api
        .issuer
        .issue(&workspace_id, &server_id, &agent_id, &service.service_id)
        .map_err(|error| {
            tracing::error!(%error, "failed to sign workload identity token");
            ApiFailure::internal("identity_signing_failed", "failed to sign identity token")
        })?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(token)).into_response())
}

pub(super) async fn agent_list_humans(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let (subject, _) = message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_HUMAN_LIST,
            PolicyResource::new(RESOURCE_HUMAN_DIRECTORY, &workspace_id),
        ))
        .await?;
    Ok(Json(json!({
        "humans": auth.list_workspace_humans(&workspace_id).await?
    })))
}

pub(super) async fn bootstrap_info(
    State(state): State<AppState>,
    Extension(config): Extension<BootstrapConfig>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    state.snapshot(&workspace_id).await?;
    let enrollment = auth
        .create_machine_enrollment(&workspace_id, &session.user_id)
        .await?;
    let (install_command, connect_command) = bootstrap_commands(&config.public_url, &enrollment);
    let script_url = install_script_url(&config.public_url);
    Ok(Json(json!({
        "install_command": install_command,
        "connect_command": connect_command,
        "enrollment_key": enrollment,
        "script_url": script_url.as_str(),
        "workspace_id": workspace_id,
    })))
}

pub(super) fn bootstrap_commands(public_url: &Url, enrollment_key: &str) -> (String, String) {
    let script_url = install_script_url(public_url);
    let install_command = format!("curl -fsSL {} | sh", shell_quote(script_url.as_str()));
    let connect_command = format!(
        "treer-agent-server connect --key {} --proxy {}",
        shell_quote(enrollment_key),
        shell_quote(public_url.as_str()),
    );
    (install_command, connect_command)
}

pub(super) async fn install_script(Extension(config): Extension<BootstrapConfig>) -> Response {
    let script = render_install_script(&config.public_url);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        script,
    )
        .into_response()
}

pub(super) async fn enroll_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
    request: Option<Json<MachineEnrollmentRequest>>,
) -> Result<Response, ApiFailure> {
    let request = request.as_ref().map(|request| &request.0);
    let enrollment = auth
        .claim_machine_enrollment_from_headers(
            &headers,
            request.map(|request| request.installation_id.as_str()),
            request.map(|request| request.name.as_str()),
            request.and_then(|request| request.existing_server_id.as_deref()),
        )
        .await?;
    state
        .allow_server_reenrollment(&enrollment.workspace_id, &enrollment.server_id)
        .await;
    let response = MachineEnrollmentResponse {
        workspace_id: enrollment.workspace_id,
        server_id: enrollment.server_id,
        machine_token: enrollment.machine_token,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(response)).into_response())
}

pub(super) async fn bind_machine_identity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(machine): Extension<MachineSession>,
    Json(request): Json<MachineEnrollmentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let workspace_id = machine.workspace_id.as_deref().ok_or_else(|| {
        ProtocolError::new(
            "machine_identity_required",
            "machine workspace identity is required",
        )
    })?;
    let server_id = machine.server_id.as_deref().ok_or_else(|| {
        ProtocolError::new(
            "machine_identity_required",
            "machine server identity is required",
        )
    })?;
    auth.bind_machine_identity(
        workspace_id,
        server_id,
        &request.installation_id,
        &request.name,
    )
    .await?;
    if state.resolve_server(workspace_id, server_id).await.is_ok() {
        let name = normalize_display_name(request.name)?;
        state.rename_server(workspace_id, server_id, name).await?;
    }
    Ok(Json(json!({ "bound": true, "server_id": server_id })))
}

pub(super) async fn download_artifact(
    Extension(config): Extension<BootstrapConfig>,
    Path((platform, binary)): Path<(String, String)>,
) -> Result<Response, ApiFailure> {
    if !valid_artifact_component(&platform)
        || !matches!(
            binary.as_str(),
            "treer" | "treer-agent-host" | "treer-agent-server"
        )
    {
        return Err(ApiFailure::not_found(
            "artifact_not_found",
            "artifact not found",
        ));
    }
    let path = config.artifacts_dir.join(&platform).join(&binary);
    match tokio::fs::read(&path).await {
        Ok(data) => Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            data,
        )
            .into_response()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let release_url = release_artifact_url(&config, &platform, &binary)?;
            tracing::info!(
                path = %path.display(),
                release = %release_url,
                "redirecting missing bootstrap artifact to release"
            );
            Ok(Redirect::temporary(release_url.as_str()).into_response())
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "bootstrap artifact unavailable");
            Err(ApiFailure::not_found(
                "artifact_not_found",
                "artifact not found",
            ))
        }
    }
}

pub(super) fn release_artifact_url(
    config: &BootstrapConfig,
    platform: &str,
    binary: &str,
) -> Result<Url, ApiFailure> {
    config
        .release_artifact_base_url
        .join(&format!("{binary}-{platform}"))
        .map_err(|error| {
            ProtocolError::new(
                "artifact_url_error",
                format!("failed to build release artifact URL: {error}"),
            )
            .into()
        })
}

pub(super) fn install_script_url(public_url: &Url) -> Url {
    let mut url = public_url.clone();
    url.set_path("/install.sh");
    url.set_query(None);
    url
}

pub(super) fn valid_artifact_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn render_install_script(public_url: &Url) -> String {
    let mut artifact_base = public_url.clone();
    artifact_base.set_path("/artifacts/");
    format!(
        r#"#!/bin/sh
set -eu

artifact_base={artifact_base}
install_dir=${{TREER_INSTALL_DIR:-"${{HOME:?HOME is required}}/.local/bin"}}
server_dir=${{TREER_AGENT_SERVER_INSTALL_DIR:-"${{HOME}}/.local/libexec/treer"}}

echo "treer: security notice" >&2
echo "treer: the Agent Server is a persistent proxy and agent host that runs with your user account's system permissions" >&2
echo "treer: workspace agents can execute commands and make network requests on this machine" >&2
echo "treer: use a dedicated account, VM, container, or other sandbox when possible" >&2

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) platform=linux-aarch64 ;;
  Darwin-x86_64|Darwin-amd64) platform=darwin-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) platform=darwin-aarch64 ;;
  *) echo "treer: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

case "$platform" in
  linux-*)
    if ! command -v unshare >/dev/null 2>&1; then
      echo "treer: warning: transparent agent networking requires unshare(1) from util-linux" >&2
    fi
    ;;
esac

if command -v curl >/dev/null 2>&1; then
  fetch() {{ curl -fsSL "$1" -o "$2"; }}
elif command -v wget >/dev/null 2>&1; then
  fetch() {{ wget -q "$1" -O "$2"; }}
else
  echo "treer: curl or wget is required" >&2
  exit 1
fi

mkdir -p "$install_dir" "$server_dir"
tmp_dir=$(mktemp -d "${{TMPDIR:-/tmp}}/treer-install.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

echo "treer: downloading $platform binaries"
fetch "$artifact_base/$platform/treer" "$tmp_dir/treer"
fetch "$artifact_base/$platform/treer-agent-host" "$tmp_dir/treer-agent-host"
fetch "$artifact_base/$platform/treer-agent-server" "$tmp_dir/treer-agent-server"
chmod 755 "$tmp_dir/treer" "$tmp_dir/treer-agent-host" "$tmp_dir/treer-agent-server"
mv "$tmp_dir/treer" "$install_dir/treer"
mv "$tmp_dir/treer-agent-host" "$server_dir/treer-agent-host"
mv "$tmp_dir/treer-agent-server" "$server_dir/treer-agent-server"
ln -sf "$server_dir/treer-agent-server" "$install_dir/treer-agent-server"

echo "treer: binaries installed"
echo "treer: add $install_dir to PATH to use treer and treer-agent-server"
echo "treer: run the workspace connection command from the Proxy UI next"
"#,
        artifact_base = shell_quote(artifact_base.as_str().trim_end_matches('/')),
    )
}
