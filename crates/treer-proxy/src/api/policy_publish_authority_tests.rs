use super::*;
use axum::body::Body;
use tower::ServiceExt;

async fn body(response: Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 128 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

fn session(user: &str) -> CurrentSession {
    CurrentSession {
        user_id: user.into(),
        token: String::new(),
        email: String::new(),
        preferred_name: user.into(),
    }
}

fn switch(
    app: &AppDeployment,
    authority: PolicyPublishAuthority,
) -> Json<SetPolicyPublishAuthorityRequest> {
    Json(SetPolicyPublishAuthorityRequest {
        expected_app_id: app.app_id.clone(),
        publish_authority: authority,
    })
}

fn document() -> WorkspacePolicyDocument {
    serde_json::from_value(json!({"schema_version":2,"defaults":{"agent.prompt":"deny"}})).unwrap()
}

#[tokio::test]
async fn default_owner_only_switch_audit_and_operator_validation() {
    let (auth, identity, app, mut headers) = policy_diagnostics::tests::fixture().await;
    headers.remove(TREER_IDENTITY_TOKEN_HEADER);
    let response = app_get_policy_publish_authority(
        Extension(auth.clone()),
        Path(app.service_id.clone()),
        headers.clone(),
    )
    .await
    .unwrap();
    let value = body(response).await;
    assert_eq!(value["publish_authority"], "owner_session");
    assert_eq!(
        value["treer_url"],
        "https://app.treer.example/orgs/org_diagnostic/workspaces/diagnostic#policy-provider"
    );
    assert_eq!(
        value["capabilities"][0],
        CAPABILITY_POLICY_PUBLISH_AUTHORITY
    );
    let failure = app_operator_validate_policy(
        Extension(auth.clone()),
        Path(app.service_id.clone()),
        headers.clone(),
        Json(document()),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error.code, "policy_owner_session_required");
    assert!(set_policy_publish_authority(
        Extension(auth.clone()),
        Extension(session("member")),
        Path("diagnostic".into()),
        switch(&app, PolicyPublishAuthority::ProviderOperator)
    )
    .await
    .is_err());
    set_policy_publish_authority(
        Extension(auth.clone()),
        Extension(session("owner")),
        Path("diagnostic".into()),
        switch(&app, PolicyPublishAuthority::ProviderOperator),
    )
    .await
    .unwrap();
    let audit: Value = sqlx::query_scalar("SELECT payload FROM organization_audit_events WHERE action='policy_provider.publish_authority.set'").fetch_one(&auth.pool()).await.unwrap();
    assert_eq!(
        audit,
        json!({"previous":"owner_session","publish_authority":"provider_operator"})
    );
    let value = body(
        app_operator_validate_policy(
            Extension(auth.clone()),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(document()),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(value["valid"], true);
    assert_eq!(
        value["capabilities"][0],
        CAPABILITY_POLICY_PUBLISH_AUTHORITY
    );
    // Switching does not relax the old owner bridge or grant access to logs.
    assert!(app_validate_policy_document(
        Extension(auth.clone()),
        Extension(identity.clone()),
        Path(app.service_id.clone()),
        headers.clone(),
        Json(document())
    )
    .await
    .is_err());
    assert!(app_policy_decisions(
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Extension(identity),
        Path(app.service_id.clone()),
        headers.clone()
    )
    .await
    .is_err());
    set_policy_publish_authority(
        Extension(auth.clone()),
        Extension(session("owner")),
        Path("diagnostic".into()),
        switch(&app, PolicyPublishAuthority::OwnerSession),
    )
    .await
    .unwrap();
    assert!(operator_provider(&auth, &app.service_id, &headers)
        .await
        .is_err());
}

#[tokio::test]
async fn app_identity_cannot_modify_authority_and_demoted_owner_is_rejected() {
    let (auth, identity, app, headers) = policy_diagnostics::tests::fixture().await;
    let router = axum::Router::new()
        .route(
            "/api/apps/{service}/policy/publish-authority",
            axum::routing::get(app_get_policy_publish_authority),
        )
        .route(
            "/api/workspaces/{workspace}/policy-provider/publish-authority",
            axum::routing::put(set_policy_publish_authority).route_layer(
                axum::middleware::from_fn_with_state(auth.clone(), crate::auth::require_user),
            ),
        )
        .layer(Extension(auth.clone()));
    let request_body =
        serde_json::to_vec(&switch(&app, PolicyPublishAuthority::ProviderOperator).0).unwrap();
    let request = |path: String, proof: HeaderMap| {
        let mut request = axum::http::Request::builder()
            .method("PUT")
            .uri(path)
            .header(header::ORIGIN, "https://app.treer.example")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(request_body.clone()))
            .unwrap();
        request.headers_mut().extend(proof);
        request
    };
    // Even a current owner's App token plus the current Provider runtime is
    // deliberately unable to mutate configuration via either API path.
    let response = router
        .clone()
        .oneshot(request(
            format!("/api/apps/{}/policy/publish-authority", app.service_id),
            headers.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let response = router
        .clone()
        .oneshot(request(
            "/api/workspaces/diagnostic/policy-provider/publish-authority".into(),
            headers.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let token = identity
        .issue_human_request("diagnostic", "owner", "Owner", "owner", &app.service_id)
        .unwrap()
        .access_token;
    let mut bearer = headers;
    bearer.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let response = router
        .oneshot(request(
            "/api/workspaces/diagnostic/policy-provider/publish-authority".into(),
            bearer,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        PolicyProviderStore::new(auth.pool())
            .get("diagnostic")
            .await
            .unwrap()
            .unwrap()
            .publish_authority,
        PolicyPublishAuthority::OwnerSession
    );
    sqlx::query("UPDATE organization_members SET role='member' WHERE user_id='owner'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert!(set_policy_publish_authority(
        Extension(auth),
        Extension(session("owner")),
        Path("diagnostic".into()),
        switch(&app, PolicyPublishAuthority::OwnerSession)
    )
    .await
    .is_err());
}

#[tokio::test]
async fn operator_rejects_invalid_rotated_revoked_runtime_and_provider_change() {
    let (auth, _, app, mut headers) = policy_diagnostics::tests::fixture().await;
    headers.remove(TREER_IDENTITY_TOKEN_HEADER);
    let store = PolicyProviderStore::new(auth.pool());
    store
        .set_publish_authority(
            "diagnostic",
            &app.app_id,
            PolicyPublishAuthority::ProviderOperator,
            "owner",
        )
        .await
        .unwrap()
        .unwrap();
    for absent in [AGENT_ID_HEADER, treer_protocol::WORKLOAD_CREDENTIAL_HEADER] {
        let mut missing = headers.clone();
        missing.remove(absent);
        assert!(operator_provider(&auth, &app.service_id, &missing)
            .await
            .is_err());
    }
    let mut other = headers.clone();
    other.insert(AGENT_ID_HEADER, "another-agent".parse().unwrap());
    assert!(operator_provider(&auth, &app.service_id, &other)
        .await
        .is_err());
    assert!(
        operator_provider(&auth, "other-workspace-service", &headers)
            .await
            .is_err()
    );
    let rotated = auth
        .rotate_agent_credential("diagnostic", "machine", &app.app_id)
        .await
        .unwrap();
    assert!(operator_provider(&auth, &app.service_id, &headers)
        .await
        .is_err());
    headers.insert(
        treer_protocol::WORKLOAD_CREDENTIAL_HEADER,
        rotated.parse().unwrap(),
    );
    assert!(operator_provider(&auth, &app.service_id, &headers)
        .await
        .is_ok());
    sqlx::query("UPDATE agent_credentials SET revoked_at='test'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert!(operator_provider(&auth, &app.service_id, &headers)
        .await
        .is_err());
    store.clear("diagnostic").await.unwrap();
    assert!(provider_runtime(&auth, &app.service_id, &headers)
        .await
        .is_err());
}

#[tokio::test]
async fn operator_explain_is_workspace_scoped_and_does_not_publish() {
    let (auth, _, app, mut headers) = policy_diagnostics::tests::fixture().await;
    headers.remove(TREER_IDENTITY_TOKEN_HEADER);
    let store = PolicyProviderStore::new(auth.pool());
    store
        .set_publish_authority(
            "diagnostic",
            &app.app_id,
            PolicyPublishAuthority::ProviderOperator,
            "owner",
        )
        .await
        .unwrap()
        .unwrap();
    for id in ["source", "target"] {
        auth.create_named_agent_credential("diagnostic", "machine", id, id, None)
            .await
            .unwrap();
    }
    let mut value = json!({"subject":{"kind":"agent","path":"/source"},"action":"agent.prompt","resource":{"kind":"agent","path":"/target"},"document":document(),"mode":"enforce"});
    let response = app_operator_explain_policy(
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Path(app.service_id.clone()),
        headers.clone(),
        Json(serde_json::from_value(value.clone()).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(
        body(response).await["explanation"]["effective_decision"],
        "deny"
    );
    value["resource"]["path"] = json!("/other-workspace-secret");
    assert!(app_operator_explain_policy(
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Path(app.service_id.clone()),
        headers.clone(),
        Json(serde_json::from_value(value).unwrap())
    )
    .await
    .is_err());
    assert!(
        treer_proxy::policy_store::WorkspacePolicyStore::new(auth.pool())
            .get("diagnostic")
            .await
            .unwrap()
            .is_none()
    );
}

fn admin_document(rules: Value) -> WorkspacePolicyDocument {
    serde_json::from_value(json!({"schema_version": 1,
        "groups": {"bots": {"principals": [{"kind": "agent", "id": "agent-a"}]}},
        "rules": rules}))
    .unwrap()
}

#[tokio::test]
async fn provider_operator_publication_cannot_add_or_change_admin_rules() {
    let (auth, identity, app, headers) = policy_diagnostics::tests::fixture().await;
    let mut runtime = headers.clone();
    runtime.remove(TREER_IDENTITY_TOKEN_HEADER);
    set_policy_publish_authority(
        Extension(auth.clone()),
        Extension(session("owner")),
        Path("diagnostic".into()),
        switch(&app, PolicyPublishAuthority::ProviderOperator),
    )
    .await
    .unwrap();
    let operator = |document: WorkspacePolicyDocument| {
        let (auth, runtime, service) = (auth.clone(), runtime.clone(), app.service_id.clone());
        async move {
            app_operator_validate_policy(Extension(auth), Path(service), runtime, Json(document))
                .await
                .map(|_| ())
                .map_err(|failure| failure.error.code)
        }
    };
    let owner = |document: WorkspacePolicyDocument, headers: HeaderMap| {
        let (auth, identity, service) = (auth.clone(), identity.clone(), app.service_id.clone());
        async move {
            app_validate_policy_document(
                Extension(auth),
                Extension(identity),
                Path(service),
                headers,
                Json(document),
            )
            .await
            .map(|_| ())
            .map_err(|failure| failure.error.code)
        }
    };
    let allow = json!({"id": "bot-admin", "priority": 10, "effect": "allow",
        "subjects": [{"group": "bots"}], "actions": ["workspace.access.manage", "machine.enroll"], "resources": [{}]});
    let deny = json!({"id": "bot-deny", "priority": 20, "effect": "deny",
        "subjects": [{"id": "agent-a"}], "actions": ["machine.enroll"], "resources": [{}]});
    let unrelated = json!({"id": "prompt", "priority": 1, "effect": "allow",
        "subjects": [{"kind": "agent"}], "actions": ["agent.prompt"], "resources": [{}]});
    let required = Err("policy_admin_owner_session_required".to_string());
    // Even with provider_operator selected, a new admin rule needs an owner.
    assert_eq!(
        operator(admin_document(json!([allow.clone()]))).await,
        required
    );
    assert_eq!(
        operator(admin_document(json!([unrelated.clone()]))).await,
        Ok(())
    );
    // An owner (who is an organization owner) approves allow + deny.
    owner(
        admin_document(json!([allow.clone(), deny.clone()])),
        headers.clone(),
    )
    .await
    .unwrap();
    // Unchanged admin rules (plus unrelated edits) pass the operator path.
    assert_eq!(
        operator(admin_document(json!([
            allow.clone(),
            deny.clone(),
            unrelated.clone()
        ])))
        .await,
        Ok(())
    );
    // Dropping an approved allow is fine; dropping the deny is not.
    assert_eq!(
        operator(admin_document(json!([deny.clone()]))).await,
        Ok(())
    );
    assert_eq!(
        operator(admin_document(json!([allow.clone()]))).await,
        required
    );
    // Changing a rule, or the group it references, is new.
    let mut wider = allow.clone();
    wider["actions"] = json!(["workspace.access.manage", "machine.enroll", "audit.read"]);
    assert_eq!(
        operator(admin_document(json!([wider, deny.clone()]))).await,
        required
    );
    let mut grown: WorkspacePolicyDocument = admin_document(json!([allow.clone(), deny.clone()]));
    grown
        .groups
        .get_mut("bots")
        .unwrap()
        .principals
        .push(serde_json::from_value(json!({"kind": "agent", "id": "agent-b"})).unwrap());
    assert_eq!(operator(grown).await, required);
    // Organization-scope admin rules need an organization owner/admin as the
    // verifying human; a mere workspace owner cannot approve them.
    sqlx::query("INSERT INTO workspace_user_grants(workspace_id,user_id,role,created_at,created_by) VALUES('diagnostic','member','owner','test','owner')")
        .execute(&auth.pool()).await.unwrap();
    let token = identity
        .issue_human_request("diagnostic", "member", "Member", "owner", &app.service_id)
        .unwrap()
        .access_token;
    let mut member = headers.clone();
    member.insert(TREER_IDENTITY_TOKEN_HEADER, token.parse().unwrap());
    let org_rule = json!({"id": "bot-org", "priority": 10, "effect": "allow",
        "subjects": [{"id": "agent-a"}], "actions": ["org.invite.manage"], "resources": [{}]});
    assert_eq!(
        owner(admin_document(json!([org_rule.clone()])), member.clone()).await,
        Err("policy_org_admin_required".to_string())
    );
    // Workspace-scope rules are fine for a workspace owner.
    owner(admin_document(json!([allow.clone()])), member.clone())
        .await
        .unwrap();
    // Once an organization owner approved the org rule, the workspace owner
    // may keep it unchanged.
    owner(admin_document(json!([org_rule.clone()])), headers.clone())
        .await
        .unwrap();
    owner(admin_document(json!([org_rule.clone(), allow])), member)
        .await
        .unwrap();
    assert_eq!(operator(admin_document(json!([org_rule]))).await, Ok(()));
}
