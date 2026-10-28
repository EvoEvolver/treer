use super::*;
use serde::Deserialize;
use treer_protocol::{PolicyPrincipalKind, WorkspacePolicyDocument, CAPABILITY_POLICY_PATH_RULES};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExplainResource {
    kind: String,
    id: Option<String>,
    path: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExplainSubject {
    kind: PolicyPrincipalKind,
    id: Option<String>,
    path: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExplainRequest {
    subject: ExplainSubject,
    action: String,
    resource: ExplainResource,
    document: Option<WorkspacePolicyDocument>,
    mode: Option<treer_protocol::PolicyMode>,
}

pub(super) async fn explain_policy(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace): Path<String>,
    Json(request): Json<ExplainRequest>,
) -> Result<Response, ApiFailure> {
    auth.require_workspace_owner(&workspace, &session.user_id)
        .await?;
    explain_policy_in_workspace(&auth, &policy, &workspace, request).await
}

pub(super) async fn explain_policy_in_workspace(
    auth: &AuthStore,
    policy: &PolicyEngine,
    workspace: &str,
    mut request: ExplainRequest,
) -> Result<Response, ApiFailure> {
    if request.action.len() > 128
        || request.action.is_empty()
        || request.action.chars().any(char::is_control)
    {
        return Err(ApiFailure::bad_request(
            "invalid_policy_action",
            "invalid policy action",
        ));
    }
    let snapshot = crate::policy_tree::TreeSnapshot::load(&auth.pool(), workspace).await?;
    let subject_id = diagnostic_id(
        &snapshot,
        match request.subject.kind {
            PolicyPrincipalKind::Agent => "agent",
            PolicyPrincipalKind::Service => "service",
            _ => "human",
        },
        request.subject.id.take(),
        request.subject.path.take(),
    )?;
    let resource_id = diagnostic_id(
        &snapshot,
        &request.resource.kind,
        request.resource.id.take(),
        request.resource.path.take(),
    )?;
    let subject = match request.subject.kind {
        PolicyPrincipalKind::Agent => snapshot.agent_subject(&subject_id).ok_or_else(|| {
            ApiFailure::bad_request(
                "policy_subject_unavailable",
                "subject not available in this workspace",
            )
        })?,
        PolicyPrincipalKind::Service => {
            if !snapshot.is_service(&subject_id) {
                return Err(ApiFailure::bad_request(
                    "policy_subject_unavailable",
                    "subject not available in this workspace",
                ));
            }
            PolicySubject::Service {
                service_id: subject_id,
            }
        }
        PolicyPrincipalKind::Human => {
            auth.require_workspace_member(workspace, &subject_id)
                .await?;
            PolicySubject::Human {
                user_id: subject_id,
            }
        }
        PolicyPrincipalKind::Machine => {
            return Err(ApiFailure::bad_request(
                "policy_subject_unsupported",
                "Tree explain supports human, Agent and App service subjects",
            ))
        }
    };
    if !matches!(
        request.resource.kind.as_str(),
        "agent" | "app" | "folder" | "agent.tree"
    ) || (request.resource.kind == "agent.tree" && resource_id != workspace)
        || (request.resource.kind != "agent.tree"
            && !snapshot.is_resource(&request.resource.kind, &resource_id))
    {
        return Err(ApiFailure::bad_request(
            "policy_resource_unavailable",
            "resource not available in this workspace",
        ));
    }
    let candidate = request.document.take();
    if let Some(document) = &candidate {
        treer_proxy::policy_store::validate_document(document)
            .map_err(|e| ApiFailure::bad_request(e.code(), &e.to_string()))?;
    }
    let mode = request.mode.unwrap_or(treer_protocol::PolicyMode::Monitor);
    let request = snapshot.enrich(&PolicyRequest::new(
        workspace,
        subject,
        request.action,
        PolicyResource::new(request.resource.kind, resource_id),
    ));
    let rescue = if matches!(
        request.action.as_str(),
        crate::policy::ACTION_AGENT_TREE_MANAGE | crate::policy::ACTION_AGENT_TREE_READ
    ) {
        if let PolicySubject::Human { user_id } = &request.subject {
            auth.require_workspace_owner(workspace, user_id)
                .await
                .is_ok()
        } else {
            false
        }
    } else {
        false
    };
    let explanation = if rescue {
        crate::policy::PolicyExplanation {
            computed_effect: treer_protocol::PolicyEffect::Allow,
            effective_decision: treer_protocol::PolicyEffect::Allow,
            mode: treer_protocol::PolicyMode::Enforce,
            matched_rule_id: None,
            reason: "owner Tree read/manage recovery".into(),
            policy_revision: 0,
            tree_revision: Some(snapshot.revision),
            source: "owner_break_glass".into(),
        }
    } else if let Some(document) = candidate {
        crate::policy::CompiledWorkspacePolicy::explain_document(document, mode, &request)
    } else {
        policy.explain_request(&request).await?
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"explanation":explanation,"capabilities":[CAPABILITY_POLICY_PATH_RULES,treer_protocol::CAPABILITY_POLICY_PUBLISH_AUTHORITY],"recovery_guaranteed":true})),
    )
        .into_response())
}

pub(super) async fn policy_decisions(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace): Path<String>,
) -> Result<Response, ApiFailure> {
    auth.require_workspace_owner(&workspace, &session.user_id)
        .await?;
    Ok(([(header::CACHE_CONTROL,"no-store")],Json(json!({"decisions":policy.decision_records(&workspace).await,"scope":"this_proxy","retention":"bounded_in_memory"}))).into_response())
}

pub(super) async fn validate_policy_document(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace): Path<String>,
    Json(document): Json<WorkspacePolicyDocument>,
) -> Result<Response, ApiFailure> {
    auth.require_workspace_owner(&workspace, &session.user_id)
        .await?;
    treer_proxy::policy_store::validate_document(&document)
        .map_err(|e| ApiFailure::bad_request(e.code(), &e.to_string()))?;
    Ok(([(header::CACHE_CONTROL,"no-store")],Json(json!({"valid":true,"schema_version":document.schema_version,"capabilities":[CAPABILITY_POLICY_PATH_RULES],"recovery_guaranteed":true,"warnings":["Owner Provider and Tree management cannot be denied by delegated rules"]}))).into_response())
}

fn diagnostic_id(
    snapshot: &crate::policy_tree::TreeSnapshot,
    kind: &str,
    id: Option<String>,
    path: Option<String>,
) -> Result<String, ApiFailure> {
    match (id, path) {
        (Some(id), None) if !id.is_empty() => Ok(id),
        (None, Some(path)) => snapshot.resolve_path(kind, &path).ok_or_else(|| {
            ApiFailure::bad_request(
                "policy_resource_unavailable",
                "path not available in this workspace",
            )
        }),
        _ => Err(ApiFailure::bad_request(
            "invalid_policy_reference",
            "specify exactly one id or canonical path",
        )),
    }
}

async fn app_owner_session(
    auth: &AuthStore,
    identity: &IdentityIssuer,
    headers: &HeaderMap,
    service: &str,
) -> Result<(String, CurrentSession), ApiFailure> {
    let token = headers
        .get(TREER_IDENTITY_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiFailure::unauthorized(
                "policy_human_token_required",
                "current human App identity token required",
            )
        })?;
    let mut human_headers = HeaderMap::new();
    human_headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().map_err(|_| {
            ApiFailure::unauthorized("policy_human_token_required", "invalid human token")
        })?,
    );
    let claims = authenticate_workspace_app(auth, identity, &human_headers, service).await?;
    if claims.principal_kind != AppPrincipalKind::Human {
        return Err(ApiFailure::forbidden(
            "policy_owner_required",
            "human workspace owner required",
        ));
    }
    auth.require_workspace_owner(&claims.workspace_id, &claims.sub)
        .await?;
    let provider = crate::policy_provider_store::PolicyProviderStore::new(auth.pool())
        .get(&claims.workspace_id)
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
                "only the configured Policy Provider may use diagnostics",
            )
        })?;
    let app = auth
        .resolve_app_deployment(&claims.workspace_id, &provider.app_id)
        .await?;
    let runtime = app.runtime_agent_id.as_deref().unwrap_or(&app.app_id);
    auth.authenticate_bound_runtime(&claims.workspace_id, &app.server_id, runtime, headers)
        .await?;
    Ok((
        claims.workspace_id,
        CurrentSession {
            token: String::new(),
            user_id: claims.sub,
            email: String::new(),
            preferred_name: claims.name,
        },
    ))
}

pub(super) async fn app_explain_policy(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service): Path<String>,
    headers: HeaderMap,
    request: Json<ExplainRequest>,
) -> Result<Response, ApiFailure> {
    let (workspace, session) = app_owner_session(&auth, &identity, &headers, &service).await?;
    explain_policy(
        Extension(auth),
        Extension(policy),
        Extension(session),
        Path(workspace),
        request,
    )
    .await
}
pub(super) async fn app_policy_decisions(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiFailure> {
    let (workspace, session) = app_owner_session(&auth, &identity, &headers, &service).await?;
    policy_decisions(
        Extension(auth),
        Extension(policy),
        Extension(session),
        Path(workspace),
    )
    .await
}
pub(super) async fn app_validate_policy_document(
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service): Path<String>,
    headers: HeaderMap,
    request: Json<WorkspacePolicyDocument>,
) -> Result<Response, ApiFailure> {
    let (workspace, session) = app_owner_session(&auth, &identity, &headers, &service).await?;
    validate_policy_document(
        Extension(auth),
        Extension(session),
        Path(workspace),
        request,
    )
    .await
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::policy_provider_store::PolicyProviderStore;
    use treer_protocol::{CreateAppDeploymentRequest, PolicyProviderFailureMode};
    use treer_proxy::policy_store::WorkspacePolicyStore;

    pub(in crate::api) async fn fixture() -> (AuthStore, IdentityIssuer, AppDeployment, HeaderMap) {
        let auth = AuthStore::for_test("test").await;
        auth.seed_test_workspace("diagnostic").await;
        sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('owner','owner@example.test',TRUE,'Owner','unused','test'),('member','member@example.test',TRUE,'Member','unused','test')").execute(&auth.pool()).await.unwrap();
        sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_diagnostic','owner','owner','test'),('org_diagnostic','member','member','test')").execute(&auth.pool()).await.unwrap();
        let app = auth
            .create_app_deployment(
                "diagnostic",
                "owner",
                "machine".into(),
                CreateAppDeploymentRequest {
                    folder_id: None,
                    server_id: Some("machine".into()),
                    name: "Policy".into(),
                    command: "python3".into(),
                    args: vec![],
                    cwd: ".".into(),
                    port: 8888,
                    hostname: String::new(),
                    public: false,
                },
            )
            .await
            .unwrap();
        let credential = auth
            .rotate_agent_credential("diagnostic", "machine", &app.app_id)
            .await
            .unwrap();
        PolicyProviderStore::new(auth.pool())
            .set(
                "diagnostic",
                &app.app_id,
                &app.service_id,
                PolicyProviderFailureMode::FailClosed,
                0,
                0,
                "owner",
            )
            .await
            .unwrap();
        let identity =
            IdentityIssuer::load(&auth, &Url::parse("https://proxy.example.test/").unwrap())
                .await
                .unwrap();
        let token = identity
            .issue_human_request("diagnostic", "owner", "Owner", "owner", &app.service_id)
            .unwrap()
            .access_token;
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, app.app_id.parse().unwrap());
        headers.insert(
            treer_protocol::WORKLOAD_CREDENTIAL_HEADER,
            credential.parse().unwrap(),
        );
        headers.insert(TREER_IDENTITY_TOKEN_HEADER, token.parse().unwrap());
        (auth, identity, app, headers)
    }

    #[tokio::test]
    async fn bridge_requires_configured_runtime_and_current_human_owner() {
        let (auth, identity, app, headers) = fixture().await;
        let bridge = |headers: HeaderMap, service: String| {
            let (auth, identity) = (auth.clone(), identity.clone());
            async move { app_owner_session(&auth, &identity, &headers, &service).await }
        };
        let service = || app.service_id.clone();
        let owner = bridge(headers.clone(), service()).await.unwrap();
        assert_eq!(owner.1.user_id, "owner");
        assert!(bridge(headers.clone(), "other-service".into())
            .await
            .is_err());
        for absent in [
            TREER_IDENTITY_TOKEN_HEADER,
            AGENT_ID_HEADER,
            treer_protocol::WORKLOAD_CREDENTIAL_HEADER,
        ] {
            let mut h = headers.clone();
            h.remove(absent);
            assert!(bridge(h, service()).await.is_err(), "without {absent}");
        }
        let mut h = headers.clone();
        h.insert(AGENT_ID_HEADER, "another-runtime".parse().unwrap());
        assert!(bridge(h, service()).await.is_err());
        let member = identity
            .issue_human_request("diagnostic", "member", "Member", "owner", &app.service_id)
            .unwrap()
            .access_token;
        let mut h = headers.clone();
        h.insert(TREER_IDENTITY_TOKEN_HEADER, member.parse().unwrap());
        assert!(bridge(h, service()).await.is_err());
        // A demoted owner or a rotated runtime credential no longer bridges.
        for role in ["member", "owner"] {
            sqlx::query("UPDATE organization_members SET role=$1 WHERE user_id='owner'")
                .bind(role)
                .execute(&auth.pool())
                .await
                .unwrap();
            assert_eq!(
                bridge(headers.clone(), service()).await.is_ok(),
                role == "owner"
            );
        }
        auth.rotate_agent_credential("diagnostic", "machine", &app.app_id)
            .await
            .unwrap();
        assert!(bridge(headers, service()).await.is_err());
    }

    #[tokio::test]
    async fn owner_simulation_resolves_paths_without_installing_candidate() {
        let (auth, identity, app, headers) = fixture().await;
        for id in ["source", "target"] {
            auth.create_named_agent_credential("diagnostic", "machine", id, id, None)
                .await
                .unwrap();
        }
        let value = json!({"subject":{"kind":"agent","path":"/source"},"action":"agent.prompt","resource":{"kind":"agent","path":"/target"},"mode":"enforce","document":{"schema_version":2,"defaults":{"agent.prompt":"deny"}}});
        let response = app_explain_policy(
            Extension(auth.clone()),
            Extension(PolicyEngine::allow_all()),
            Extension(identity.clone()),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(serde_json::from_value(value.clone()).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["explanation"]["effective_decision"], "deny");
        assert!(WorkspacePolicyStore::new(auth.pool())
            .get("diagnostic")
            .await
            .unwrap()
            .is_none());
        let mut missing = value;
        missing["resource"]["path"] = json!("/other-workspace-secret");
        assert!(app_explain_policy(
            Extension(auth.clone()),
            Extension(PolicyEngine::allow_all()),
            Extension(identity.clone()),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(serde_json::from_value(missing).unwrap())
        )
        .await
        .is_err());
        let mut h = headers;
        h.remove(TREER_IDENTITY_TOKEN_HEADER);
        assert!(app_policy_decisions(
            Extension(auth),
            Extension(PolicyEngine::allow_all()),
            Extension(identity),
            Path(app.service_id),
            h
        )
        .await
        .is_err());
    }

    #[test]
    fn simulation_rejects_forged_context_attributes_and_ambiguous_refs() {
        let base = json!({"subject":{"kind":"agent","id":"a"},"action":"agent.prompt","resource":{"kind":"agent","id":"b"}});
        for field in ["tree", "attributes"] {
            let mut value = base.clone();
            value["resource"][field] = json!({"path":"/admin"});
            assert!(serde_json::from_value::<ExplainRequest>(value).is_err());
        }
        let mut value = base;
        value["subject"]["tree"] = json!({});
        assert!(serde_json::from_value::<ExplainRequest>(value).is_err());
    }

    #[tokio::test]
    async fn owner_tree_recovery_bypasses_deny_all_and_unavailable_provider() {
        let (auth, _, _, _) = fixture().await;
        let store = WorkspacePolicyStore::new(auth.pool());
        store.replace("diagnostic",0,treer_protocol::PolicyMode::Enforce,serde_json::from_value(json!({"schema_version":2,"defaults":{"agent.tree.manage":"deny","agent.tree.read":"deny"}})).unwrap(),treer_protocol::PolicyPrincipalRef{kind:PolicyPrincipalKind::Human,id:"owner".into()}).await.unwrap();
        let state = AppState::new();
        let unavailable = PolicyEngine::durable_with_provider(
            store.clone(),
            PolicyProviderStore::new(auth.pool()),
            auth.clone(),
            state.clone(),
        );
        let owner = CurrentSession {
            token: String::new(),
            user_id: "owner".into(),
            email: String::new(),
            preferred_name: String::new(),
        };
        let member = CurrentSession {
            user_id: "member".into(),
            ..owner.clone()
        };
        for engine in [PolicyEngine::durable(store), unavailable] {
            for action in [
                crate::policy::ACTION_AGENT_TREE_MANAGE,
                crate::policy::ACTION_AGENT_TREE_READ,
            ] {
                super::super::workload_tree::authorize_tree(
                    &state,
                    &auth,
                    &engine,
                    Some(&owner),
                    None,
                    &HeaderMap::new(),
                    "diagnostic",
                    action,
                )
                .await
                .unwrap();
                assert!(super::super::workload_tree::authorize_tree(
                    &state,
                    &auth,
                    &engine,
                    Some(&member),
                    None,
                    &HeaderMap::new(),
                    "diagnostic",
                    action
                )
                .await
                .is_err());
            }
        }
        for action in [
            crate::policy::ACTION_AGENT_TREE_MANAGE,
            crate::policy::ACTION_AGENT_TREE_READ,
        ] {
            let candidate:ExplainRequest=serde_json::from_value(json!({"subject":{"kind":"human","id":"owner"},"action":action,"resource":{"kind":"agent.tree","id":"diagnostic"},"document":{"schema_version":2,"defaults":{action:"deny"}},"mode":"enforce"})).unwrap();
            let response = explain_policy(
                Extension(auth.clone()),
                Extension(PolicyEngine::allow_all()),
                Extension(owner.clone()),
                Path("diagnostic".into()),
                Json(candidate),
            )
            .await
            .unwrap();
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body["explanation"]["source"], "owner_break_glass");
            assert_eq!(body["explanation"]["effective_decision"], "allow");
        }
    }
}
