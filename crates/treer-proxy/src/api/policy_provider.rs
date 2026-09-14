use super::*;
use crate::policy_provider_store::PolicyProviderStore;
use treer_protocol::{
    InstallDefaultPolicyAppRequest, MachineServiceProtocol, PolicyProviderBundle,
    PolicyProviderInvalidationRequest, PolicyProviderInvalidationResponse, PolicyProviderManifest,
    SetWorkspacePolicyProviderRequest, POLICY_PROVIDER_BUNDLE_CAPABILITY_V1,
    POLICY_PROVIDER_PROTOCOL_V1,
};

const POLICY_PROVIDER_MANIFEST_MAX_BYTES: u32 = 32 * 1024;
const POLICY_PROVIDER_MANIFEST_TIMEOUT_MS: u64 = 3_000;
const POLICY_PROVIDER_BUNDLE_MAX_BYTES: u32 = 300 * 1024;
const MAX_POLICY_PROVIDER_STALE_SECONDS: u64 = 86_400;
const DEFAULT_POLICY_APP_NAME: &str = "Treer Policy";
const DEFAULT_POLICY_APP_COMMAND: &str = "python3";
const DEFAULT_POLICY_APP_SCRIPT: &str = "treer-policy.py";
const DEFAULT_POLICY_APP_PORT_START: u16 = 8_787;
const DEFAULT_POLICY_APP_PORT_END: u16 = 8_899;
const DEFAULT_POLICY_APP_FILES: &[(&str, &[u8])] = &[
    (
        "treer-policy-agent.md",
        include_bytes!("../../../../apps/policy/AGENT.md"),
    ),
    (
        "treer-policy-index.html",
        include_bytes!("../../../../apps/policy/web/index.html"),
    ),
    (
        "treer-policy-app.css",
        include_bytes!("../../../../apps/policy/web/app.css"),
    ),
    (
        "treer-policy-app.js",
        include_bytes!("../../../../apps/policy/web/app.js"),
    ),
    (
        DEFAULT_POLICY_APP_SCRIPT,
        include_bytes!("../../../../apps/policy/policy.py"),
    ),
];

pub(super) async fn get_policy_provider(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<PolicyProviderStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let provider = store.get(&workspace_id).await.map_err(|error| {
        ApiFailure::internal("policy_provider_store_failed", &error.to_string())
    })?;
    let mut app = match &provider {
        Some(provider) => Some(
            auth.resolve_app_deployment(&workspace_id, &provider.app_id)
                .await?,
        ),
        None => None,
    };
    if let Some(app) = app.as_mut() {
        hydrate_app_deployment(&state, app).await;
        hydrate_app_public_url(&auth, &config, app).await?;
    }
    let cache = policy.provider_cache_status(&workspace_id).await;
    let fallback = if provider.is_none() {
        let stored = treer_proxy::policy_store::WorkspacePolicyStore::new(auth.pool())
            .get(&workspace_id)
            .await
            .map_err(|error| ApiFailure::internal(error.code(), &error.to_string()))?;
        Some(match stored {
            Some(stored) => json!({
                "kind": "workspace_policy",
                "name": "Stored Workspace Policy",
                "mode": stored.mode,
                "revision": stored.revision,
            }),
            None => json!({
                "kind": "treer_default",
                "name": "Treer Default",
                "mode": "monitor",
                "effect": "allow",
            }),
        })
    } else {
        None
    };
    Ok(Json(
        json!({ "provider": provider, "app": app, "cache": cache, "fallback": fallback }),
    ))
}

pub(super) async fn set_policy_provider(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<PolicyProviderStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<SetWorkspacePolicyProviderRequest>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_owner(&workspace_id, &session.user_id)
        .await?;
    if request.max_stale_seconds > MAX_POLICY_PROVIDER_STALE_SECONDS {
        return Err(ApiFailure::bad_request(
            "invalid_policy_provider_stale_window",
            "Policy Provider stale window must be at most 86400 seconds",
        ));
    }
    let mut app = auth
        .resolve_app_deployment(&workspace_id, request.app_id.trim())
        .await?;
    hydrate_app_deployment(&state, &mut app).await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    let has_public_ingress = auth
        .list_service_ingresses(&workspace_id)
        .await?
        .iter()
        .any(|ingress| {
            ingress.service_id == app.service_id
                && ingress.enabled
                && ingress.access == ServiceIngressAccess::Public
        });
    if app.access == Some(ServiceIngressAccess::Public) || has_public_ingress {
        return Err(ApiFailure::bad_request(
            "policy_provider_must_be_private",
            "Policy Provider App must require a workspace session",
        ));
    }
    let service = auth
        .resolve_machine_service(&workspace_id, &app.service_id)
        .await?;
    if service.protocol != MachineServiceProtocol::Http
        || service.server_id != app.server_id
        || service.target_port != app.port
        || service.target_host != "127.0.0.1"
    {
        return Err(ApiFailure::bad_request(
            "invalid_policy_provider_service",
            "Policy Provider must be a standard Managed App HTTP service",
        ));
    }
    let manifest_value = state
        .send_command(
            &workspace_id,
            &app.server_id,
            AgentCommand::FetchServiceHttp {
                port: app.port,
                path: "/v1/manifest".to_string(),
                max_bytes: POLICY_PROVIDER_MANIFEST_MAX_BYTES,
                timeout_ms: POLICY_PROVIDER_MANIFEST_TIMEOUT_MS,
            },
        )
        .await?;
    let manifest: PolicyProviderManifest =
        serde_json::from_value(manifest_value).map_err(|error| {
            ApiFailure::bad_request("invalid_policy_provider_manifest", &error.to_string())
        })?;
    if manifest.protocol != POLICY_PROVIDER_PROTOCOL_V1
        || !manifest
            .capabilities
            .iter()
            .any(|capability| capability == POLICY_PROVIDER_BUNDLE_CAPABILITY_V1)
    {
        return Err(ApiFailure::bad_request(
            "unsupported_policy_provider",
            "App does not advertise the Treer Policy Provider v1 bundle capability",
        ));
    }
    let query = {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("workspace_id", &workspace_id);
        query.finish()
    };
    let bundle_value = state
        .send_command(
            &workspace_id,
            &app.server_id,
            AgentCommand::FetchServiceHttp {
                port: app.port,
                path: format!("/v1/policy/bundle?{query}"),
                max_bytes: POLICY_PROVIDER_BUNDLE_MAX_BYTES,
                timeout_ms: POLICY_PROVIDER_MANIFEST_TIMEOUT_MS,
            },
        )
        .await?;
    let bundle: PolicyProviderBundle = serde_json::from_value(bundle_value).map_err(|error| {
        ApiFailure::bad_request("invalid_policy_provider_bundle", &error.to_string())
    })?;
    if bundle.protocol != POLICY_PROVIDER_PROTOCOL_V1 || bundle.workspace_id != workspace_id {
        return Err(ApiFailure::bad_request(
            "invalid_policy_provider_bundle",
            "Policy App returned a bundle with the wrong protocol or workspace",
        ));
    }
    treer_proxy::policy_store::validate_document(&bundle.document)
        .map_err(|error| ApiFailure::bad_request(error.code(), &error.to_string()))?;
    let provider = store
        .set(
            &workspace_id,
            &app.app_id,
            &app.service_id,
            request.failure_mode,
            request.max_stale_seconds,
            bundle.revision,
            &session.user_id,
        )
        .await
        .map_err(|error| {
            ApiFailure::internal("policy_provider_store_failed", &error.to_string())
        })?;
    policy.invalidate_provider(&workspace_id).await;
    record_policy_provider_audit(
        &auth,
        &session,
        "policy_provider.configured",
        &workspace_id,
        &app,
        json!({
            "failure_mode": provider.failure_mode,
            "max_stale_seconds": provider.max_stale_seconds,
            "protocol": manifest.protocol,
        }),
    )
    .await;
    let cache = policy.provider_cache_status(&workspace_id).await;
    Ok(Json(
        json!({ "provider": provider, "app": app, "manifest": manifest, "cache": cache }),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn install_default_policy_app(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<PolicyProviderStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<InstallDefaultPolicyAppRequest>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_owner(&workspace_id, &session.user_id)
        .await?;
    if store
        .get(&workspace_id)
        .await
        .map_err(|error| ApiFailure::internal("policy_provider_store_failed", &error.to_string()))?
        .is_some()
    {
        return Err(ApiFailure {
            status: StatusCode::CONFLICT,
            error: ProtocolError::new(
                "policy_provider_already_configured",
                "remove the current Policy Provider before installing the default App",
            ),
        });
    }

    let existing = auth
        .list_app_deployments(&workspace_id)
        .await?
        .into_iter()
        .find(is_default_policy_app);
    let server_id = match &existing {
        Some(app) if app.server_id != request.server_id => {
            return Err(ApiFailure::bad_request(
                "default_policy_app_machine_mismatch",
                "the existing default Policy App belongs to another machine",
            ));
        }
        Some(app) => app.server_id.clone(),
        None => {
            state
                .select_server(&workspace_id, Some(request.server_id.trim()))
                .await?
        }
    };

    for (file_name, contents) in DEFAULT_POLICY_APP_FILES {
        let _ = upload_machine_file(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Some(Extension(session.clone())),
            None,
            HeaderMap::new(),
            Path((workspace_id.clone(), server_id.clone())),
            Json(UploadMachineFileRequest {
                directory: String::new(),
                file_name: (*file_name).to_string(),
                content_base64: base64::engine::general_purpose::STANDARD.encode(contents),
                overwrite: true,
            }),
        )
        .await?;
    }

    let app = if let Some(app) = existing {
        let Json(value) = restart_app_deployment(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(config.clone()),
            Extension(policy.clone()),
            Some(Extension(session.clone())),
            None,
            HeaderMap::new(),
            Path((workspace_id.clone(), app.app_id)),
        )
        .await?;
        app_from_response(value)?
    } else {
        let services = auth.list_machine_services(&workspace_id).await?;
        let port = (DEFAULT_POLICY_APP_PORT_START..=DEFAULT_POLICY_APP_PORT_END)
            .find(|port| {
                !services
                    .iter()
                    .any(|service| service.server_id == server_id && service.target_port == *port)
            })
            .ok_or_else(|| {
                ApiFailure::service_unavailable(
                    "default_policy_app_port_unavailable",
                    "no port is available for the default Policy App",
                )
            })?;
        let Json(value) = create_app_deployment(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(config.clone()),
            Extension(policy.clone()),
            Some(Extension(session.clone())),
            None,
            HeaderMap::new(),
            Path(workspace_id.clone()),
            Json(CreateAppDeploymentRequest {
                server_id: Some(server_id),
                name: DEFAULT_POLICY_APP_NAME.to_string(),
                command: DEFAULT_POLICY_APP_COMMAND.to_string(),
                args: vec![
                    DEFAULT_POLICY_APP_SCRIPT.to_string(),
                    "--port".to_string(),
                    port.to_string(),
                ],
                cwd: String::new(),
                port,
                hostname: "policy.internal".to_string(),
                public: false,
            }),
        )
        .await?;
        app_from_response(value)?
    };

    let mut last_error = None;
    for _ in 0..20 {
        match set_policy_provider(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(config.clone()),
            Extension(policy.clone()),
            Extension(store.clone()),
            Extension(session.clone()),
            Path(workspace_id.clone()),
            Json(SetWorkspacePolicyProviderRequest {
                app_id: app.app_id.clone(),
                failure_mode: treer_protocol::PolicyProviderFailureMode::FailClosed,
                max_stale_seconds: 300,
            }),
        )
        .await
        {
            Ok(response) => return Ok(response),
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Err(last_error.unwrap_or_else(|| {
        ApiFailure::service_unavailable(
            "default_policy_app_unavailable",
            "default Policy App did not become ready",
        )
    }))
}

fn is_default_policy_app(app: &AppDeployment) -> bool {
    app.name == DEFAULT_POLICY_APP_NAME
        && app.command == DEFAULT_POLICY_APP_COMMAND
        && app
            .args
            .first()
            .is_some_and(|arg| arg == DEFAULT_POLICY_APP_SCRIPT)
}

fn app_from_response(value: Value) -> Result<AppDeployment, ApiFailure> {
    serde_json::from_value(value.get("app").cloned().unwrap_or(Value::Null)).map_err(|error| {
        ApiFailure::internal(
            "invalid_app_response",
            &format!("Managed App operation returned invalid data: {error}"),
        )
    })
}

pub(super) async fn clear_policy_provider(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<PolicyProviderStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_owner(&workspace_id, &session.user_id)
        .await?;
    let previous = store.get(&workspace_id).await.map_err(|error| {
        ApiFailure::internal("policy_provider_store_failed", &error.to_string())
    })?;
    let cleared = store.clear(&workspace_id).await.map_err(|error| {
        ApiFailure::internal("policy_provider_store_failed", &error.to_string())
    })?;
    policy.invalidate_provider(&workspace_id).await;
    if let Some(previous) = previous {
        if let Ok(app) = auth
            .resolve_app_deployment(&workspace_id, &previous.app_id)
            .await
        {
            record_policy_provider_audit(
                &auth,
                &session,
                "policy_provider.removed",
                &workspace_id,
                &app,
                json!({}),
            )
            .await;
        }
    }
    Ok(Json(json!({ "cleared": cleared })))
}

pub(super) async fn invalidate_policy_provider(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<PolicyProviderStore>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<PolicyProviderInvalidationRequest>,
) -> Result<Json<PolicyProviderInvalidationResponse>, ApiFailure> {
    if request.revision == 0 {
        return Err(ApiFailure::bad_request(
            "invalid_policy_provider_revision",
            "Policy Provider revision must be greater than zero",
        ));
    }
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let agent_id = match subject {
        PolicySubject::Agent { agent_id, .. } => agent_id,
        _ => {
            return Err(ApiFailure::forbidden(
                "policy_provider_app_required",
                "only the configured Policy App may invalidate its cache",
            ))
        }
    };
    let provider = store
        .get(&workspace_id)
        .await
        .map_err(|error| ApiFailure::internal("policy_provider_store_failed", &error.to_string()))?
        .ok_or_else(|| {
            ApiFailure::not_found(
                "policy_provider_not_found",
                "workspace has no Policy Provider",
            )
        })?;
    let app = auth
        .resolve_app_deployment(&workspace_id, &provider.app_id)
        .await?;
    if app.runtime_agent_id.as_deref() != Some(agent_id.as_str()) {
        return Err(ApiFailure::forbidden(
            "policy_provider_app_required",
            "only the configured Policy App may invalidate its cache",
        ));
    }
    let accepted_revision = store
        .advance_revision(&workspace_id, &provider.app_id, request.revision)
        .await
        .map_err(|error| ApiFailure::internal("policy_provider_store_failed", &error.to_string()))?
        .ok_or_else(|| {
            ApiFailure::not_found(
                "policy_provider_not_found",
                "workspace has no Policy Provider",
            )
        })?;
    policy.invalidate_provider(&workspace_id).await;
    Ok(Json(PolicyProviderInvalidationResponse {
        accepted_revision,
    }))
}

async fn record_policy_provider_audit(
    auth: &AuthStore,
    session: &CurrentSession,
    action: &'static str,
    workspace_id: &str,
    app: &AppDeployment,
    payload: Value,
) {
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id,
            actor_kind: "human",
            actor_id: Some(&session.user_id),
            action,
            resource_kind: "policy_provider",
            resource_id: &app.app_id,
            resource_name: Some(&app.name),
            payload,
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, action, "failed to record Policy Provider audit event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_app_bundle_is_complete_and_script_is_installed_last() {
        let names = DEFAULT_POLICY_APP_FILES
            .iter()
            .map(|(name, _)| *name)
            .collect::<HashSet<_>>();
        assert_eq!(names.len(), DEFAULT_POLICY_APP_FILES.len());
        assert!(DEFAULT_POLICY_APP_FILES
            .iter()
            .all(|(_, contents)| !contents.is_empty()));
        assert_eq!(
            DEFAULT_POLICY_APP_FILES.last().map(|(name, _)| *name),
            Some(DEFAULT_POLICY_APP_SCRIPT)
        );
    }
}
