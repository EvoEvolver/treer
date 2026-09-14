use super::*;
use crate::policy_provider_store::PolicyProviderStore;
use treer_protocol::{
    MachineServiceProtocol, PolicyProviderBundle, PolicyProviderInvalidationRequest,
    PolicyProviderInvalidationResponse, PolicyProviderManifest, SetWorkspacePolicyProviderRequest,
    POLICY_PROVIDER_BUNDLE_CAPABILITY_V1, POLICY_PROVIDER_PROTOCOL_V1,
};

const POLICY_PROVIDER_MANIFEST_MAX_BYTES: u32 = 32 * 1024;
const POLICY_PROVIDER_MANIFEST_TIMEOUT_MS: u64 = 3_000;
const POLICY_PROVIDER_BUNDLE_MAX_BYTES: u32 = 300 * 1024;
const MAX_POLICY_PROVIDER_STALE_SECONDS: u64 = 86_400;

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
    Ok(Json(
        json!({ "provider": provider, "app": app, "cache": cache }),
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
