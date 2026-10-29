use super::*;
use chrono::{Duration, Utc};
use tower::ServiceExt;
use treer_protocol::{PolicyMode, PolicyPrincipalKind, PolicyPrincipalRef};
use treer_proxy::policy_store::WorkspacePolicyStore;

pub(super) async fn fixture() -> AuthStore {
    let auth = AuthStore::for_test("audit-test").await;
    auth.seed_test_workspace("a").await;
    sqlx::query("INSERT INTO workspaces(workspace_id,organization_id,name,created_at,created_by,access_mode) VALUES('b','org_a','B','test','test','restricted')")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('owner','owner@example.test',TRUE,'Owner','unused','test'),('member','member@example.test',TRUE,'Member','unused','test'),('admin','admin@example.test',TRUE,'Admin','unused','test'),('local-owner','local@example.test',TRUE,'Local','unused','test')")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_a','owner','owner','test'),('org_a','admin','admin','test'),('org_a','member','member','test'),('org_a','local-owner','member','test')")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO workspace_user_grants(workspace_id,user_id,role,created_at,created_by) VALUES('a','local-owner','owner','test','owner')")
        .execute(&auth.pool()).await.unwrap();
    for user in ["owner", "member", "admin", "local-owner"] {
        sqlx::query(
            "INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES($1,$2,$3,$4)",
        )
        .bind(format!("audit-session-{user}"))
        .bind(user)
        .bind(Utc::now().to_rfc3339())
        .bind((Utc::now() + Duration::hours(1)).to_rfc3339())
        .execute(&auth.pool())
        .await
        .unwrap();
    }
    auth
}

fn human(user: &str) -> PolicySubject {
    PolicySubject::Human {
        user_id: user.into(),
    }
}

pub(super) fn test_router(auth: &AuthStore, policy: PolicyEngine) -> Router {
    routes(auth.clone())
        .layer(Extension(auth.clone()))
        .layer(Extension(policy))
        .with_state(AppState::new())
}

pub(super) async fn response(router: Router, uri: &str, user: Option<&str>) -> Response {
    let mut request = Request::builder().uri(uri);
    if let Some(user) = user {
        request = request.header(
            header::COOKIE,
            format!("treer_session=audit-session-{user}"),
        );
    }
    router
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

pub(super) async fn owner_get(router: &Router, uri: &str) -> Value {
    let result = response(router.clone(), uri, Some("owner")).await;
    assert_eq!(result.status(), StatusCode::OK, "{uri}");
    body(result).await
}

pub(super) async fn body(response: Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn event(
    auth: &AuthStore,
    workspace: Option<&str>,
    actor: &str,
    action: &str,
    payload: Value,
) {
    let mut tx = auth.pool().begin().await.unwrap();
    crate::audit::insert(
        &mut tx,
        crate::audit::NewAuditEvent {
            organization_id: "org_a",
            workspace_id: workspace,
            actor_kind: "user",
            actor_id: Some(actor),
            source: "test",
            action,
            resource_kind: "agent",
            resource_id: "ag_target",
            resource_name: Some("not projected"),
            payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn workspace_audit_http_role_scope_filters_pagination_and_redaction() {
    let auth = fixture().await;
    event(&auth, Some("a"), "owner", "agent.create", json!({"server_id":"srv_one","argv":["SECRET"],"env":{"TOKEN":"SECRET"},"config":"SECRET","body":"SECRET","path":"SECRET","sha256":"SECRET"})).await;
    event(
        &auth,
        Some("a"),
        "member",
        "machine.exec",
        json!({"exit_code":0,"timed_out":false,"cwd":"SECRET","stdout":"SECRET"}),
    )
    .await;
    event(
        &auth,
        Some("b"),
        "owner",
        "agent.delete",
        json!({"body":"FOREIGN"}),
    )
    .await;
    event(
        &auth,
        None,
        "owner",
        "organization.invite",
        json!({"token":"ORG_SECRET"}),
    )
    .await;
    let router = test_router(&auth, PolicyEngine::allow_all());
    let stats = owner_get(
        &router,
        "/api/workspaces/a/audit-events?include_statistics=true",
    )
    .await;
    let stats = &stats["maintenance"];
    assert_eq!(stats["retention_days"], 180);
    assert!(stats["deleted_through"].is_null());
    let daily = stats["rows_per_day"].as_array().unwrap();
    assert_eq!(daily.len(), 2);
    assert!(daily
        .iter()
        .all(|row| row["action"] != "agent.delete" && row["action"] != "organization.invite"));
    assert!(stats.get("overflow_total").is_none());
    let denied = response(
        router.clone(),
        "/api/workspaces/a/audit-events?include_statistics=true",
        Some("member"),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    for (user, status) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("member"), StatusCode::FORBIDDEN),
        (Some("owner"), StatusCode::OK),
        (Some("admin"), StatusCode::OK),
        (Some("local-owner"), StatusCode::OK),
    ] {
        let result = response(router.clone(), "/api/workspaces/a/audit-events", user).await;
        assert_eq!(result.status(), status, "{user:?}");
        assert_eq!(result.headers()[header::CACHE_CONTROL], "no-store");
    }
    let forbidden = response(
        router.clone(),
        "/api/workspaces/b/audit-events",
        Some("local-owner"),
    )
    .await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let first = owner_get(&router, "/api/workspaces/a/audit-events?limit=1").await;
    assert_eq!(first["events"].as_array().unwrap().len(), 1);
    assert_eq!(first["events"][0]["action"], "machine.exec");
    let cursor = first["next_cursor"].as_i64().unwrap();
    let second = owner_get(
        &router,
        &format!("/api/workspaces/a/audit-events?limit=1&before={cursor}"),
    )
    .await;
    assert!(second["next_cursor"].is_null());
    assert_eq!(
        second["events"][0]["payload"],
        json!({"server_id":"srv_one"})
    );
    for page in [&first, &second] {
        let encoded = page.to_string();
        assert!(
            !encoded.contains("SECRET")
                && !encoded.contains("FOREIGN")
                && !encoded.contains("not projected")
        );
        assert!(page["events"][0].get("actor_name").is_none());
        assert_eq!(page["events"][0]["workspace_id"], "a");
    }
    for query in [
        "actor_id=member",
        "action=machine.exec",
        "resource_kind=agent&actor_id=member",
        "since=2000-01-01T00%3A00%3A00Z",
        "until=2100-01-01T00%3A00%3A00Z",
    ] {
        let page = owner_get(&router, &format!("/api/workspaces/a/audit-events?{query}")).await;
        assert!(!page["events"].as_array().unwrap().is_empty());
        assert!(page["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["workspace_id"] == "a"));
    }
    // Old organization view still includes organization-wide NULL events and
    // its original payload, even with a workspace filter.
    let old = crate::audit::list(&auth.pool(), "org_a", Some("a"), None, 100)
        .await
        .unwrap();
    assert_eq!(old.len(), 3);
    assert!(old.iter().any(|e| e.payload.get("token").is_some()));
    let boundary = first["events"][0]["occurred_at"]
        .as_str()
        .unwrap()
        .replace('+', "%2B");
    for (filter, expected) in [("since", "machine.exec"), ("until", "agent.create")] {
        let page = owner_get(
            &router,
            &format!("/api/workspaces/a/audit-events?{filter}={boundary}"),
        )
        .await;
        assert_eq!(page["events"].as_array().unwrap().len(), 1);
        assert_eq!(page["events"][0]["action"], expected);
    }
    let empty = owner_get(
        &router,
        "/api/workspaces/a/audit-events?action=machine.exec&actor_id=owner",
    )
    .await;
    assert!(empty["events"].as_array().unwrap().is_empty());
    for query in [
        "workspace_id=b",
        "before=0",
        "before=-1",
        "action=x%27OR%201=1",
        "since=bad",
        "since=2026-01-02T00:00:00Z&until=2026-01-01T00:00:00Z",
        "since=2000-01-01T00:00:00Z&until=2100-01-01T00:00:00Z",
    ] {
        let result = response(
            router.clone(),
            &format!("/api/workspaces/a/audit-events?{query}"),
            Some("owner"),
        )
        .await;
        assert_eq!(result.status(), StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(result.headers()[header::CACHE_CONTROL], "no-store");
    }
}

#[tokio::test]
async fn workspace_audit_page_and_payload_are_bounded_and_unknown_actions_fail_closed() {
    let auth = fixture().await;
    for _ in 0..101 {
        event(
            &auth,
            Some("a"),
            "owner",
            "agent.create",
            json!({"server_id":"srv_safe","startup_revision":2,"body":"SECRET".repeat(100_000)}),
        )
        .await;
    }
    let router = test_router(&auth, PolicyEngine::allow_all());
    let page = owner_get(&router, "/api/workspaces/a/audit-events?limit=65535").await;
    assert_eq!(page["events"].as_array().unwrap().len(), 100);
    assert!(page["next_cursor"].is_i64());
    assert!(page.to_string().len() < 100_000);
    let single = owner_get(&router, "/api/workspaces/a/audit-events?limit=0").await;
    assert_eq!(single["events"].as_array().unwrap().len(), 1);
    event(
        &auth,
        Some("a"),
        "owner",
        "machine.file.write",
        json!({"server_id":"srv_safe","path":"SECRET","sha256":"SECRET"}),
    )
    .await;
    event(
        &auth,
        Some("a"),
        "owner",
        "future.action",
        json!({"server_id":"srv_safe","startup_revision":2,"token":"SECRET"}),
    )
    .await;
    event(
        &auth,
        Some("a"),
        "owner",
        "machine.exec",
        json!({"exit_code":{"token":"SECRET"},"timed_out":"SECRET","truncated":false}),
    )
    .await;
    let page = owner_get(&router, "/api/workspaces/a/audit-events?limit=3").await;
    assert_eq!(page["events"][0]["payload"], json!({"truncated":false}));
    assert_eq!(page["events"][1]["payload"], json!({}));
    assert_eq!(page["events"][2]["payload"], json!({}));
    assert!(!page.to_string().contains("SECRET"));
}

#[tokio::test]
async fn workspace_audit_requires_explicit_enforced_grant_and_owner_deny_applies() {
    let auth = fixture().await;
    let store = WorkspacePolicyStore::new(auth.pool());
    let subjects = [
        human("member"),
        PolicySubject::Agent {
            server_id: "srv_one".into(),
            agent_id: "agent_one".into(),
        },
        PolicySubject::Service {
            service_id: "svc_one".into(),
        },
        PolicySubject::Machine {
            server_id: "srv_one".into(),
        },
    ];
    for subject in &subjects {
        assert_eq!(
            read_audit(
                &auth,
                &PolicyEngine::allow_all(),
                "a",
                subject.clone(),
                Default::default()
            )
            .await
            .unwrap_err()
            .status,
            StatusCode::FORBIDDEN
        );
    }
    let allow = json!({"schema_version":2,"rules":[{"id":"audit-grant","priority":0,"effect":"allow","subjects":[{}],"actions":["workspace.audit.read"],"resources":[{"kind":"workspace","id":"a"}]}]});
    for (revision, mode, document, succeeds) in [
        (0, PolicyMode::Monitor, allow.clone(), false),
        (
            1,
            PolicyMode::Enforce,
            json!({"schema_version":2,"defaults":{"workspace.audit.read":"allow"},"rules":[]}),
            false,
        ),
        (2, PolicyMode::Enforce, allow, true),
    ] {
        store
            .replace(
                "a",
                revision,
                mode,
                serde_json::from_value(document).unwrap(),
                PolicyPrincipalRef {
                    kind: PolicyPrincipalKind::Human,
                    id: "owner".into(),
                },
            )
            .await
            .unwrap();
        let engine = PolicyEngine::durable(store.clone());
        for subject in &subjects {
            let result = read_audit(&auth, &engine, "a", subject.clone(), Default::default()).await;
            assert_eq!(result.is_ok(), succeeds, "{mode:?} {subject:?}");
        }
        assert!(
            read_audit(&auth, &engine, "a", human("owner"), Default::default())
                .await
                .is_ok()
        );
    }
    store
        .replace(
            "a",
            3,
            PolicyMode::Enforce,
            serde_json::from_value(
                json!({"schema_version":2,"defaults":{"workspace.audit.read":"deny"},"rules":[]}),
            )
            .unwrap(),
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let result = read_audit(
        &auth,
        &PolicyEngine::durable(store),
        "a",
        human("owner"),
        Default::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(result.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn workspace_audit_app_tokens_bind_the_caller_and_audience() {
    let auth = fixture().await;
    let app = auth
        .create_app_deployment(
            "a",
            "owner",
            "srv_one".into(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("srv_one".into()),
                name: "Audit".into(),
                command: "node".into(),
                args: vec![],
                cwd: ".".into(),
                port: 8888,
                hostname: String::new(),
                public: false,
                ui_capabilities: Vec::new(),
            },
        )
        .await
        .unwrap();
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://proxy.example.test/").unwrap())
        .await
        .unwrap();
    let router = test_router(&auth, PolicyEngine::allow_all()).layer(Extension(identity.clone()));
    for (user, audience, status) in [
        ("owner", app.service_id.as_str(), StatusCode::OK),
        ("member", app.service_id.as_str(), StatusCode::FORBIDDEN),
        ("owner", "svc_other", StatusCode::UNAUTHORIZED),
    ] {
        let token = identity
            .issue_human_request("a", user, user, "owner", audience)
            .unwrap()
            .access_token;
        let result = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/apps/{}/audit-events", app.service_id))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), status, "{user} {audience}");
        assert_eq!(result.headers()[header::CACHE_CONTROL], "no-store");
    }
}

#[tokio::test]
async fn workspace_audit_agent_authentication_cannot_spoof_or_cross_workspace() {
    let auth = fixture().await;
    let enrollment = auth.create_machine_enrollment("a", "owner").await.unwrap();
    let machine = auth.claim_machine_enrollment(&enrollment).await.unwrap();
    let credential = auth
        .create_named_agent_credential("a", &machine.server_id, "agent_one", "audit-agent", None)
        .await
        .unwrap();
    let store = WorkspacePolicyStore::new(auth.pool());
    let router = test_router(&auth, PolicyEngine::allow_all());
    let request = |workspace: &str, id: &str, secret: &str| {
        Request::builder()
            .uri(format!("/agent/workspaces/{workspace}/audit-events"))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", machine.machine_token),
            )
            .header(AGENT_ID_HEADER, id)
            .header(treer_protocol::WORKLOAD_CREDENTIAL_HEADER, secret)
            .body(Body::empty())
            .unwrap()
    };
    for (workspace, id, secret, status) in [
        ("a", "agent_one", credential.as_str(), StatusCode::FORBIDDEN),
        ("b", "agent_one", credential.as_str(), StatusCode::FORBIDDEN),
        ("a", "owner", credential.as_str(), StatusCode::UNAUTHORIZED),
        ("a", "agent_one", "wlc_forged", StatusCode::UNAUTHORIZED),
    ] {
        let result = router
            .clone()
            .oneshot(request(workspace, id, secret))
            .await
            .unwrap();
        assert_eq!(result.status(), status);
        assert_eq!(result.headers()[header::CACHE_CONTROL], "no-store");
    }
    store.replace("a", 0, PolicyMode::Enforce, serde_json::from_value(json!({
        "schema_version":2,"rules":[{"id":"only-agent","priority":0,"effect":"allow",
            "subjects":[{"kind":"agent","id":"agent_one"}],"actions":["workspace.audit.read"],
            "resources":[{"kind":"workspace","id":"a"}]}]
    })).unwrap(), PolicyPrincipalRef { kind:PolicyPrincipalKind::Human, id:"owner".into() }).await.unwrap();
    let router = test_router(&auth, PolicyEngine::durable(store));
    assert_eq!(
        router
            .clone()
            .oneshot(request("a", "agent_one", &credential))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let machine_only = Request::builder()
        .uri("/agent/workspaces/a/audit-events")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", machine.machine_token),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(machine_only).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
