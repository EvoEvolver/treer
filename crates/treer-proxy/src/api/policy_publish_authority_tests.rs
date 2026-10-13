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
    assert_eq!(value["treer_url"], auth.app_public_url().as_str());
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
async fn binding_fence_legacy_writes_reset_and_audit_failure_rolls_back() {
    let (auth, _, app, _) = policy_diagnostics::tests::fixture().await;
    let store = PolicyProviderStore::new(auth.pool());
    assert!(store
        .set_publish_authority(
            "diagnostic",
            "stale-app",
            PolicyPublishAuthority::ProviderOperator,
            "owner"
        )
        .await
        .unwrap()
        .is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM organization_audit_events")
        .fetch_one(&auth.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
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
    // Same App reconfiguration preserves the owner's explicit choice.
    store
        .set(
            "diagnostic",
            &app.app_id,
            &app.service_id,
            treer_protocol::PolicyProviderFailureMode::FailClosed,
            0,
            0,
            "owner",
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get("diagnostic")
            .await
            .unwrap()
            .unwrap()
            .publish_authority,
        PolicyPublishAuthority::ProviderOperator
    );
    // Even old writers that omit the column trigger a reset on service change.
    sqlx::query("INSERT INTO machine_services(service_id,workspace_id,name,server_id,target_host,target_port,protocol,created_at,created_by,updated_at,updated_by) SELECT 'other-service',workspace_id,'other','machine','127.0.0.1',8889,'http',created_at,created_by,updated_at,updated_by FROM machine_services WHERE service_id=$1").bind(&app.service_id).execute(&auth.pool()).await.unwrap();
    sqlx::query("UPDATE workspace_policy_providers SET service_id='other-service' WHERE workspace_id='diagnostic'").execute(&auth.pool()).await.unwrap();
    assert_eq!(
        store
            .get("diagnostic")
            .await
            .unwrap()
            .unwrap()
            .publish_authority,
        PolicyPublishAuthority::OwnerSession
    );
    store
        .set(
            "diagnostic",
            &app.app_id,
            &app.service_id,
            treer_protocol::PolicyProviderFailureMode::FailClosed,
            0,
            0,
            "owner",
        )
        .await
        .unwrap();
    let other = auth
        .create_app_deployment(
            "diagnostic",
            "owner",
            "machine".into(),
            treer_protocol::CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine".into()),
                name: "Other Policy".into(),
                command: "python3".into(),
                args: vec![],
                cwd: ".".into(),
                port: 8890,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
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
    sqlx::query("UPDATE workspace_policy_providers SET app_id=$1, service_id=$2 WHERE workspace_id='diagnostic'").bind(&other.app_id).bind(&other.service_id).execute(&auth.pool()).await.unwrap();
    assert_eq!(
        store
            .get("diagnostic")
            .await
            .unwrap()
            .unwrap()
            .publish_authority,
        PolicyPublishAuthority::OwnerSession
    );
    store
        .set_publish_authority(
            "diagnostic",
            &other.app_id,
            PolicyPublishAuthority::ProviderOperator,
            "owner",
        )
        .await
        .unwrap()
        .unwrap();
    store.clear("diagnostic").await.unwrap();
    store
        .set(
            "diagnostic",
            &app.app_id,
            &app.service_id,
            treer_protocol::PolicyProviderFailureMode::FailClosed,
            0,
            0,
            "owner",
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get("diagnostic")
            .await
            .unwrap()
            .unwrap()
            .publish_authority,
        PolicyPublishAuthority::OwnerSession
    );
    // Failure to record the switch cannot leave the switch enabled.
    sqlx::raw_sql("CREATE FUNCTION reject_authority_audit() RETURNS TRIGGER LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test audit unavailable'; END $$; CREATE TRIGGER reject_authority_audit BEFORE INSERT ON organization_audit_events FOR EACH ROW EXECUTE FUNCTION reject_authority_audit();").execute(&auth.pool()).await.unwrap();
    assert!(store
        .set_publish_authority(
            "diagnostic",
            &app.app_id,
            PolicyPublishAuthority::ProviderOperator,
            "owner"
        )
        .await
        .is_err());
    assert_eq!(
        store
            .get("diagnostic")
            .await
            .unwrap()
            .unwrap()
            .publish_authority,
        PolicyPublishAuthority::OwnerSession
    );
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

#[tokio::test]
async fn dedicated_paths_fail_on_old_routers_and_new_paths_require_authentication() {
    let (auth, _, app, _) = policy_diagnostics::tests::fixture().await;
    // An old Proxy has no dedicated endpoint; the legacy PUT cannot absorb it.
    let old = axum::Router::new().route(
        "/api/workspaces/{workspace}/policy-provider",
        axum::routing::put(|| async { StatusCode::OK }),
    );
    let response = old
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri("/api/workspaces/diagnostic/policy-provider/publish-authority")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let new = axum::Router::new()
        .route(
            "/api/apps/{service}/policy/publish-authority",
            axum::routing::get(app_get_policy_publish_authority),
        )
        .layer(Extension(auth));
    let response = new
        .oneshot(
            axum::http::Request::builder()
                .uri(format!(
                    "/api/apps/{}/policy/publish-authority",
                    app.service_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
