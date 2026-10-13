use super::*;
use crate::policy_provider_store::PolicyProviderStore;
use treer_protocol::{
    PolicyPublishAuthority, SetPolicyPublishAuthorityRequest, WorkspacePolicyDocument,
    WorkspacePolicyProvider, CAPABILITY_POLICY_PATH_RULES, CAPABILITY_POLICY_PUBLISH_AUTHORITY,
};

#[cfg(test)]
#[path = "policy_publish_authority_tests.rs"]
mod tests;

fn authority_response(provider: WorkspacePolicyProvider, treer_url: &str) -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "publish_authority": provider.publish_authority,
            "provider": provider,
            "treer_url": treer_url,
            "capabilities": [CAPABILITY_POLICY_PUBLISH_AUTHORITY],
            "supported_schema_versions": [1, 2]
        })),
    )
        .into_response()
}

// Called only after authenticating this exact workspace's owner or the bound
// Provider runtime. Do not enumerate organizations/workspaces to build a link.
async fn settings_url(auth: &AuthStore, workspace: &str) -> Result<String, ApiFailure> {
    let organization: Option<String> = sqlx::query_scalar(
        "SELECT organization_id FROM workspaces WHERE workspace_id=$1 AND deleted_at IS NULL",
    )
    .bind(workspace)
    .fetch_optional(&auth.pool())
    .await
    .map_err(|_| {
        ApiFailure::internal(
            "policy_provider_store_failed",
            "cannot resolve settings target",
        )
    })?;
    let organization = organization
        .ok_or_else(|| ApiFailure::not_found("workspace_not_found", "workspace not available"))?;
    let mut url = auth.app_public_url().clone();
    url.path_segments_mut()
        .map_err(|_| ApiFailure::internal("invalid_console_url", "invalid control console URL"))?
        .clear()
        .push("orgs")
        .push(&organization)
        .push("workspaces")
        .push(workspace);
    url.set_query(None);
    url.set_fragment(Some("policy-provider"));
    Ok(url.into())
}

pub(super) async fn set_policy_publish_authority(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace): Path<String>,
    Json(request): Json<SetPolicyPublishAuthorityRequest>,
) -> Result<Response, ApiFailure> {
    // Provider administration is an unconditional owner-only recovery boundary;
    // never ask supplied Policy whether a non-owner may change this switch.
    auth.require_workspace_owner(&workspace, &session.user_id)
        .await?;
    let provider = PolicyProviderStore::new(auth.pool())
        .set_publish_authority(
            &workspace,
            &request.expected_app_id,
            request.publish_authority,
            &session.user_id,
        )
        .await
        .map_err(|_| {
            ApiFailure::internal(
                "policy_provider_store_failed",
                "cannot persist publication authority and audit",
            )
        })?
        .ok_or_else(|| {
            ApiFailure::bad_request(
                "policy_provider_changed",
                "expected App is no longer the selected Provider",
            )
        })?;
    let url = settings_url(&auth, &workspace).await?;
    Ok(authority_response(provider, &url))
}

async fn provider_runtime(
    auth: &AuthStore,
    service: &str,
    headers: &HeaderMap,
) -> Result<WorkspacePolicyProvider, ApiFailure> {
    // Resolve by immutable service ID, not a self-reported workspace or name.
    let workspace: Option<String> = sqlx::query_scalar(
        "SELECT workspace_id FROM workspace_policy_providers WHERE service_id=$1",
    )
    .bind(service)
    .fetch_optional(&auth.pool())
    .await
    .map_err(|_| {
        ApiFailure::internal(
            "policy_provider_store_failed",
            "cannot read Provider binding",
        )
    })?;
    let workspace = workspace.ok_or_else(|| {
        ApiFailure::forbidden(
            "policy_provider_required",
            "current configured Provider runtime required",
        )
    })?;
    let provider = PolicyProviderStore::new(auth.pool())
        .get(&workspace)
        .await
        .map_err(|_| {
            ApiFailure::internal(
                "policy_provider_store_failed",
                "cannot read Provider binding",
            )
        })?
        .filter(|p| p.service_id == service)
        .ok_or_else(|| {
            ApiFailure::forbidden(
                "policy_provider_required",
                "current configured Provider runtime required",
            )
        })?;
    let app = auth
        .resolve_app_deployment(&workspace, &provider.app_id)
        .await?;
    let runtime = app.runtime_agent_id.as_deref().unwrap_or(&app.app_id);
    auth.authenticate_bound_runtime(&workspace, &app.server_id, runtime, headers)
        .await?;
    // Re-read after credential verification: do not reuse an authority read
    // before authentication while an owner may have changed the binding.
    PolicyProviderStore::new(auth.pool())
        .get(&workspace)
        .await
        .map_err(|_| {
            ApiFailure::internal(
                "policy_provider_store_failed",
                "cannot read Provider binding",
            )
        })?
        .filter(|current| current.app_id == provider.app_id && current.service_id == service)
        .ok_or_else(|| {
            ApiFailure::forbidden(
                "policy_provider_required",
                "Provider changed during authentication",
            )
        })
}

async fn operator_provider(
    auth: &AuthStore,
    service: &str,
    headers: &HeaderMap,
) -> Result<WorkspacePolicyProvider, ApiFailure> {
    let provider = provider_runtime(auth, service, headers).await?;
    if provider.publish_authority != PolicyPublishAuthority::ProviderOperator {
        return Err(ApiFailure::forbidden(
            "policy_owner_session_required",
            "owner has not enabled provider_operator publication",
        ));
    }
    Ok(provider)
}

pub(super) async fn app_get_policy_publish_authority(
    Extension(auth): Extension<AuthStore>,
    Path(service): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiFailure> {
    let provider = provider_runtime(&auth, &service, &headers).await?;
    let url = settings_url(&auth, &provider.workspace_id).await?;
    Ok(authority_response(provider, &url))
}

pub(super) async fn app_operator_validate_policy(
    Extension(auth): Extension<AuthStore>,
    Path(service): Path<String>,
    headers: HeaderMap,
    Json(document): Json<WorkspacePolicyDocument>,
) -> Result<Response, ApiFailure> {
    operator_provider(&auth, &service, &headers).await?;
    treer_proxy::policy_store::validate_document(&document)
        .map_err(|e| ApiFailure::bad_request(e.code(), &e.to_string()))?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "valid":true,"schema_version":document.schema_version,
            "publish_authority":"provider_operator",
            "capabilities":[CAPABILITY_POLICY_PUBLISH_AUTHORITY,CAPABILITY_POLICY_PATH_RULES],
            "recovery_guaranteed":true
        })),
    )
        .into_response())
}

pub(super) async fn app_operator_explain_policy(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Path(service): Path<String>,
    headers: HeaderMap,
    Json(request): Json<policy_diagnostics::ExplainRequest>,
) -> Result<Response, ApiFailure> {
    let provider = operator_provider(&auth, &service, &headers).await?;
    policy_diagnostics::explain_policy_in_workspace(&auth, &policy, &provider.workspace_id, request)
        .await
}
