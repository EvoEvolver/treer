use super::*;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::process::{Command, Stdio};
use tower::ServiceExt;
use treer_protocol::{
    CommandResult, MachineServiceProtocol, PolicyEffect, PolicyMode, PolicyPrincipalKind,
    PolicyPrincipalRef, ProxyMessage, WorkspacePolicyDocument, POLICY_SCHEMA_VERSION,
};
use treer_proxy::policy_store::WorkspacePolicyStore;

/// Durable enforce-mode Policy whose defaults deny `actions` (revision `revision`).
async fn deny_policy(auth: &AuthStore, revision: u64, actions: &[&str]) -> PolicyEngine {
    let store = WorkspacePolicyStore::new(auth.pool());
    store
        .replace(
            "default",
            revision,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: actions
                    .iter()
                    .map(|action| (action.to_string(), PolicyEffect::Deny))
                    .collect(),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    PolicyEngine::durable(store)
}

/// A Policy that denies every action.
fn deny_all() -> PolicyEngine {
    PolicyEngine::new(
        crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
            "policy_denied",
            "denied by fixture",
        )),
        vec![],
    )
}

/// The complete Core router over `auth`/`state` with an allow-all Policy.
async fn full_router(auth: AuthStore, state: AppState, rollout: CapabilityRollout) -> Router {
    router_with(
        auth,
        state,
        rollout,
        crate::updater::UpdaterClient::disabled(),
    )
    .await
}

async fn router_with(
    auth: AuthStore,
    state: AppState,
    rollout: CapabilityRollout,
    updater: crate::updater::UpdaterClient,
) -> Router {
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://treer.example/").unwrap())
        .await
        .unwrap();
    router(
        state,
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        rollout,
        updater,
    )
}

/// Sends a request with the Console origin; returns status and JSON body (Null if empty).
async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::ORIGIN, "https://app.treer.example");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn machine_a() -> Option<Extension<MachineSession>> {
    Some(Extension(MachineSession {
        server_id: Some("machine-a".into()),
        workspace_id: Some("default".into()),
    }))
}

fn agent_a_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    headers
}

#[tokio::test]
async fn app_tree_browser_and_workload_messages_preserve_owner_and_nested_identity() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    sqlx::raw_sql("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('owner230','owner230@example.test','Owner230','fixture','now'); INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','owner230','owner','now'); INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES('session230','owner230','2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')")
        .execute(&auth.pool()).await.unwrap();
    let app = auth
        .create_app_deployment(
            "default",
            "owner230",
            "machine-a".into(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".into()),
                name: "hub230".into(),
                command: "fixture".into(),
                args: vec![],
                cwd: ".".into(),
                port: 8081,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    let owner = app.runtime_agent_id.unwrap();
    let state = state_with_managed_agent().await;
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let router = full_router(auth.clone(), state.clone(), CapabilityRollout::new(true)).await;
    let cookie = Some("treer_session=session230");
    let send = "/api/workspaces/default/messages";
    let input = json!({"recipients":[format!("{owner}/team/tools/agent1"),format!("{owner}/team/tools/agent2"),owner],"body":"nested offline","idempotency_key":"tree230"});
    let (status, _) = call(&router, Method::POST, send, None, Some(input.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A valid session must not bypass the cookie-Origin check.
    for origin in [None, Some("https://untrusted.example")] {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(send)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, "treer_session=session230");
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::from(input.to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let (status, sent) = call(&router, Method::POST, send, cookie, Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let sent: treer_protocol::SendMessageResponse = serde_json::from_value(sent).unwrap();
    assert_eq!(sent.delivery_ids.len(), 3);
    assert_eq!(sent.message.sender.kind, MessagePrincipalKind::Human);
    assert_eq!(
        sent.message.recipients[0].app_endpoint.as_deref(),
        Some("team/tools/agent1")
    );
    let mut spoof = input.clone();
    spoof["sender_endpoint"] = json!("team/tools/agent1");
    let (status, _) = call(&router, Method::POST, send, cookie, Some(spoof)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for target in [
        "agent-b/team/tools/agent1",
        "hub230/../agent1",
        "hub230/a//agent1",
    ] {
        let invalid = json!({"recipients":[target],"body":"no","idempotency_key":target});
        let (status, _) = call(&router, Method::POST, send, cookie, Some(invalid)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let principal = |id: &str, name: &str| MessagePrincipal {
        app_endpoint: None,
        path: None,
        kind: MessagePrincipalKind::Agent,
        id: id.into(),
        name: name.into(),
        role: None,
    };
    let incoming = messages
        .receive("default", &principal(&owner, "hub230"), 10, 0)
        .await
        .unwrap();
    assert_eq!(
        incoming.deliveries.len(),
        3,
        "offline App retains distinct subtree deliveries"
    );
    let reply: SendMessageRequest = serde_json::from_value(json!({"recipients":["owner230"],"body":"actual child","context_ids":[sent.message.message_id],"sender_endpoint":"team/tools/agent1","idempotency_key":"reply230"})).unwrap();
    let allow = PolicyEngine::allow_all();
    let reply_as = |agent_id: &str, name: &str| {
        send_message_for(
            &state,
            &auth,
            &allow,
            &messages,
            "default",
            PolicySubject::Agent {
                server_id: "machine-a".into(),
                agent_id: agent_id.into(),
            },
            principal(agent_id, name),
            reply.clone(),
        )
    };
    let answered = reply_as(&owner, "hub230").await.unwrap();
    assert_eq!(
        answered.message.sender.app_endpoint.as_deref(),
        Some("team/tools/agent1")
    );
    // Only the owning App may speak for its endpoints.
    assert_eq!(
        reply_as("agent-b", "reviewer").await.unwrap_err().status,
        StatusCode::FORBIDDEN
    );
    let page = format!("/api/workspaces/default/messages?app_owner_id={owner}&app_endpoint=team%2Ftools%2Fagent1&limit=1");
    let (status, body) = call(&router, Method::GET, &page, cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    let body: treer_protocol::MessagePage = serde_json::from_value(body).unwrap();
    assert_eq!(body.messages[0].message_id, answered.message.message_id);
    assert!(body.next_cursor.is_some());
    sqlx::query("DELETE FROM organization_members WHERE user_id='owner230'")
        .execute(&auth.pool())
        .await
        .unwrap();
    // A retained cookie cannot read or mutate after current membership removal.
    let (status, _) = call(&router, Method::POST, send, cookie, Some(input)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&router, Method::GET, &page, cookie, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn reservation_cleanup_requires_owned_record_policy_and_durable_fence() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    auth.seed_test_workspace("other").await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('creator','creator@example.test','Creator','fixture','now'),('other','other@example.test','Other','fixture','now')")
        .execute(&auth.pool()).await.unwrap();
    for workspace in ["default", "other"] {
        for user in ["creator", "other"] {
            sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES($1,$2,'member','now')")
                .bind(format!("org_{workspace}")).bind(user).execute(&auth.pool()).await.unwrap();
        }
    }
    auth.reserve_named_agent(
        "default",
        "machine-a",
        "ag_candidate",
        "reserved-name",
        None,
        None,
        Some("creator"),
    )
    .await
    .unwrap();
    let state = state_with_managed_agent().await;
    let server = state.resolve_server("default", "machine-a").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "cleanup-fixture".into(),
            vec![treer_protocol::CAPABILITY_AGENT_RESERVATION_FENCE.into()],
            tx,
        )
        .await
        .unwrap();
    let call = |workspace: &str, user: &str, id: &str, policy: PolicyEngine| {
        let (state, auth) = (state.clone(), auth.clone());
        let (workspace, user, id) = (workspace.to_owned(), user.to_owned(), id.to_owned());
        async move {
            cleanup_agent_reservation(
                State(state),
                Extension(auth),
                Extension(policy),
                Extension(CurrentSession {
                    token: "fixture-session".into(),
                    user_id: user,
                    email: "fixture@example.test".into(),
                    preferred_name: "Fixture".into(),
                }),
                Path((workspace, id)),
                axum::body::Bytes::new(),
            )
            .await
        }
    };
    // Another member, another workspace, or an absent id: indistinguishable 404.
    for (workspace, user, id) in [
        ("default", "other", "ag_candidate"),
        ("other", "creator", "ag_candidate"),
        ("default", "creator", "ag_absent"),
    ] {
        let error = call(workspace, user, id, PolicyEngine::allow_all())
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.error.code, "agent_reservation_not_found");
    }
    let denied = deny_policy(&auth, 0, &[ACTION_AGENT_DELETE]).await;
    assert_eq!(
        call("default", "creator", "ag_candidate", denied)
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
    assert!(
        rx.try_recv().is_err(),
        "ownership/Policy denials must not contact Controller"
    );
    let allow = deny_policy(&auth, 1, &[]).await;
    let responder = tokio::spawn({
        let state = state.clone();
        async move {
            let envelope = next_lifecycle_command(&mut rx).await;
            assert!(
                matches!(&envelope.command, AgentCommand::FenceAbsentAgentReservation { agent_id } if agent_id=="ag_candidate")
            );
            state
                .complete_command(CommandResult::success(
                    envelope.command_id,
                    json!({"fenced":true}),
                ))
                .await;
        }
    });
    let result = call("default", "creator", "ag_candidate", allow.clone()).await;
    responder.await.unwrap();
    assert_eq!(
        result.unwrap().0,
        json!({"agent_id":"ag_candidate","deleted":true,"already_deleted":false})
    );
    let repeat = call("default", "creator", "ag_candidate", allow)
        .await
        .unwrap()
        .0;
    assert_eq!(repeat["already_deleted"], true);
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT revoked_at IS NOT NULL FROM agent_credentials WHERE agent_id='ag_candidate'"
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap());
}

#[tokio::test]
async fn agent_create_policy_denial_precedes_reservation() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let request = || CreateAgentRequest {
        folder_id: None,
        server_id: Some("machine-a".into()),
        kind: "command".into(),
        name: "not-reserved".into(),
        cwd: ".".into(),
        args: vec!["claude".into()],
        cols: 80,
        rows: 24,
        publish_ports: vec![],
        recipe: None,
    };
    let policy = deny_all();
    let subject = PolicySubject::Agent {
        server_id: "machine-a".into(),
        agent_id: "agent-a".into(),
    };
    let error = execute_agent_create(
        &state,
        &auth,
        &policy,
        None,
        Some(&subject),
        "default",
        request(),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "policy_denied");
    assert!(error.error.details.is_none());

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_names WHERE name='not-reserved'")
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn app_directory_and_resolution_do_not_enumerate_path_denied_agents() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('owner','owner@example.test',TRUE,'Owner','unused','test'),('other','other@example.test',TRUE,'Other','unused','test')").execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','owner','owner','test'),('org_default','other','member','test')").execute(&auth.pool()).await.unwrap();
    let app = auth
        .create_app_deployment(
            "default",
            "owner",
            "machine-a".into(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".into()),
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
    auth.create_named_agent_credential("default", "machine-a", "agent-a", "agent-a", None)
        .await
        .unwrap();
    auth.create_named_agent_credential("default", "machine-a", "agent-b", "reviewer", None)
        .await
        .unwrap();
    let store = WorkspacePolicyStore::new(auth.pool());
    let doc=serde_json::from_value(json!({"schema_version":2,"defaults":{"agent.tree.read":"deny","agent.discover":"deny","agent.metadata.read":"deny","human.list":"deny"},"rules":[{"id":"one","priority":0,"effect":"allow","subjects":[{"kind":"human","id":"owner"}],"actions":["agent.discover","agent.metadata.read"],"resources":[{"kind":"agent","tree":{"path_eq":"/agent-a"}}]}]})).unwrap();
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            doc,
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let policy = PolicyEngine::durable(store);
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://proxy.example.test/").unwrap())
        .await
        .unwrap();
    let token = identity
        .issue_human_request("default", "owner", "Owner", "owner", &app.service_id)
        .unwrap()
        .access_token;
    let headers = HeaderMap::from_iter([(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    )]);
    let state = state_with_managed_agent().await;
    let Json(directory) = workspace_app_directory(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(identity.clone()),
        Path(app.service_id.clone()),
        headers.clone(),
    )
    .await
    .unwrap();
    let serialized = directory.to_string();
    assert!(serialized.contains("agent-a"));
    assert_eq!(
        directory["capabilities"][0],
        treer_protocol::APP_DIRECTORY_PATH_CAPABILITY
    );
    let caller = directory["principals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "agent-a")
        .unwrap();
    // Discovery/metadata allow the principal, but Tree read denial redacts path.
    assert!(caller["path"].is_null());
    assert!(serialized.contains("owner"));
    assert!(!serialized.contains("reviewer"));
    assert!(!serialized.contains("other"));
    let request: ResolveAppRecipientsRequest =
        serde_json::from_value(json!({"recipients":["reviewer"]})).unwrap();
    let error = resolve_workspace_app_recipients(
        State(state),
        Extension(auth),
        Extension(policy),
        Extension(identity),
        Path(app.service_id),
        headers,
        Json(request),
    )
    .await
    .unwrap_err();
    assert!(!error.error.message.contains("agent-b"));
}

#[tokio::test]
async fn prompt_queue_policy_denies_send_and_listing_before_dispatch() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let policy = deny_policy(
        &auth,
        0,
        &[ACTION_AGENT_PROMPT, crate::policy::ACTION_AGENT_QUEUE_READ],
    )
    .await;
    let headers = agent_a_headers();
    let machine = machine_a();
    let error = prompt_agent(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        machine.clone(),
        headers.clone(),
        Path(("default".into(), "reviewer".into())),
        Json(treer_protocol::SubmitAgentPromptRequest {
            text: "task".into(),
            delivery: treer_protocol::PromptDelivery::Queue,
            notify: None,
            require_receipt: false,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "policy_denied");
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM organization_audit_events WHERE action='agent.prompt'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(audit_count, 0);
    let error = read_prompt_queue(
        State(state),
        Extension(auth),
        Extension(policy),
        machine,
        headers,
        Path(("default".into(), "reviewer".into())),
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "policy_denied");
}

#[tokio::test]
async fn prompt_queue_attests_sender_and_checks_cancel_ownership() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let server = state.resolve_server("default", "machine-a").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "queue-test".into(),
            vec![treer_protocol::CAPABILITY_AGENT_PROMPT_DELIVERY.into()],
            tx,
        )
        .await
        .unwrap();
    let policy = deny_policy(
        &auth,
        0,
        &[
            crate::policy::ACTION_AGENT_QUEUE_READ,
            crate::policy::ACTION_AGENT_QUEUE_CANCEL,
        ],
    )
    .await;
    let headers = agent_a_headers();
    let machine = machine_a();
    let pending = tokio::spawn({
        let (state, auth, policy, headers, machine) = (
            state.clone(),
            auth.clone(),
            policy.clone(),
            headers.clone(),
            machine.clone(),
        );
        async move {
            crate::audit::MUTATION_ACTOR
                .scope(
                    crate::audit::MutationActor {
                        kind: "agent",
                        id: Some("agent-a".into()),
                    },
                    prompt_agent(
                        State(state),
                        Extension(auth),
                        Extension(policy),
                        machine,
                        headers,
                        Path(("default".into(), "reviewer".into())),
                        Json(treer_protocol::SubmitAgentPromptRequest {
                            text: "task".into(),
                            delivery: treer_protocol::PromptDelivery::Queue,
                            notify: Some(treer_protocol::PromptNotify::default()),
                            require_receipt: true,
                        }),
                    ),
                )
                .await
        }
    });
    let envelope = next_lifecycle_command(&mut rx).await;
    match envelope.command {
        AgentCommand::SubmitPrompt {
            agent_id,
            sender: Some(sender),
            request,
        } => {
            assert_eq!(agent_id, "agent-b");
            assert_eq!(sender.agent_id, "agent-a");
            assert_eq!(sender.path, "/agent-a");
            assert_eq!(sender.name, "agent-a");
            assert_eq!(request.notify.unwrap().to.as_deref(), Some("agent-a"));
            assert!(request.require_receipt);
        }
        command => panic!("unexpected command: {command:?}"),
    }
    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            json!({"submission":{"id":"sub-1","disposition":"queued","position":1}}),
        ))
        .await;
    assert!(pending.await.unwrap().is_ok());
    for sender in ["agent-b", "agent-a"] {
        let pending = tokio::spawn({
            let (state, auth, policy, headers, machine) = (
                state.clone(),
                auth.clone(),
                policy.clone(),
                headers.clone(),
                machine.clone(),
            );
            async move {
                cancel_prompt_submission(
                    State(state),
                    Extension(auth),
                    Extension(policy),
                    machine,
                    headers,
                    Path(("default".into(), "reviewer".into(), "sub-1".into())),
                )
                .await
            }
        });
        let read = next_lifecycle_command(&mut rx).await;
        assert!(matches!(read.command, AgentCommand::SubmissionRead { .. }));
        state
            .complete_command(CommandResult::success(
                read.command_id,
                json!({"submission":{"id":"sub-1","sender":{"agent_id":sender}}}),
            ))
            .await;
        if sender == "agent-b" {
            assert_eq!(
                pending.await.unwrap().unwrap_err().error.code,
                "policy_denied"
            );
            assert!(rx.try_recv().is_err());
        } else {
            let cancel = next_lifecycle_command(&mut rx).await;
            assert!(matches!(cancel.command, AgentCommand::QueueCancel { .. }));
            state
                .complete_command(CommandResult::success(
                    cancel.command_id,
                    json!({"submission":{"id":"sub-1","state":"interrupted"}}),
                ))
                .await;
            assert_eq!(
                pending.await.unwrap().unwrap().0["submission"]["state"],
                "interrupted"
            );
        }
    }
}

#[test]
fn original_startup_stays_self_only() {
    let source = PolicySubject::Agent {
        server_id: "machine-a".into(),
        agent_id: "agent-a".into(),
    };
    assert!(require_self_agent(&source, "agent-a").is_ok());
    assert!(require_self_agent(&source, "agent-b").is_err());
    assert!(require_self_agent(
        &PolicySubject::Human {
            user_id: "owner".into()
        },
        "agent-a"
    )
    .is_err());
}

#[tokio::test]
async fn admin_startup_cross_agent_policy_denies_before_controller_dispatch() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let policy = deny_policy(&auth, 0, &[ACTION_AGENT_STARTUP_MANAGE]).await;
    let headers = agent_a_headers();
    let machine = machine_a();
    let error = set_admin_agent_startup(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        None,
        machine.clone(),
        headers.clone(),
        Path(("default".into(), "reviewer".into())),
        Json(ReplaceAgentStartupRequest {
            cwd: None,
            command: "sh".into(),
            args: vec![],
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
    let error = get_admin_agent_startup(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        None,
        machine.clone(),
        headers.clone(),
        Path(("default".into(), "agent-b".into())),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
    let error = restart_agent_with_startup(
        State(state),
        Extension(auth),
        Extension(policy),
        None,
        machine,
        headers,
        Path(("default".into(), "agent-b".into())),
        Json(ReplaceAgentStartupRequest {
            cwd: None,
            command: "sh".into(),
            args: vec![],
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
}

pub(super) async fn state_with_managed_agent() -> AppState {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
        capabilities: vec![],
    };
    let agent = treer_protocol::AgentInfo {
        path: None,
        run_generation: 0,
        agent_id: "agent-a".to_string(),
        workspace_id: "default".to_string(),
        server_id: "machine-a".to_string(),
        kind: "command".to_string(),
        name: "agent-a".to_string(),
        cwd: ".".to_string(),
        status: treer_protocol::AgentStatus::Idle,
        pid: None,
        started_at: now,
        updated_at: now,
        exited_at: None,
        exit_code: None,
        output_revision: 0,
        interface: None,
    };
    let mut recipient = agent.clone();
    recipient.agent_id = "agent-b".to_string();
    recipient.name = "reviewer".to_string();
    let connection_id = Uuid::new_v4();
    let (outgoing, _incoming) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, outgoing)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![agent, recipient],
            },
        )
        .await
        .expect("apply agent snapshot");
    state
}

#[tokio::test]
async fn machine_file_transfer_policy_denies_both_directions_before_dispatch() {
    use treer_protocol::{MachineFileTransferCall, MachineFileTransferRequest as Transfer};
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let policy = deny_policy(
        &auth,
        0,
        &[
            ACTION_MACHINE_FILE_WRITE,
            crate::policy::ACTION_MACHINE_FILE_READ,
        ],
    )
    .await;
    let headers = agent_a_headers();
    let machine = machine_a();
    for request in [
        Transfer::GetBegin {
            path: "result".into(),
            limit: 64,
        },
        Transfer::PutBegin {
            path: "result".into(),
            overwrite: false,
            mode: 0o600,
            size: Some(3),
            limit: 64,
        },
    ] {
        let error = machine_file_transfer(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            None,
            machine.clone(),
            headers.clone(),
            Path(("default".into(), "machine-a".into())),
            Json(MachineFileTransferCall {
                path: "result".into(),
                request,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.error.code, "policy_denied");
    }
}

fn workload_tree_agent_router(state: AppState, auth: AuthStore, policy: PolicyEngine) -> Router {
    Router::new()
        .route(
            "/agent/workspaces/{workspace_id}/workload-tree",
            get(get_workload_tree),
        )
        .route(
            "/agent/workspaces/{workspace_id}/workload-tree/folders",
            post(create_workload_folder),
        )
        .route(
            "/agent/workspaces/{workspace_id}/workload-tree/folders/{folder_id}",
            patch(update_workload_folder).delete(delete_workload_folder),
        )
        .route(
            "/agent/workspaces/{workspace_id}/workload-tree/placements/{agent_id}",
            axum::routing::put(set_workload_placement),
        )
        .route_layer(middleware::from_fn_with_state(
            auth.clone(),
            auth::require_machine,
        ))
        .layer(Extension(auth))
        .layer(Extension(policy))
        .with_state(state)
}

async fn tree_request(
    router: &Router,
    headers: &HeaderMap,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.headers_mut().extend(headers.clone());
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn workload_tree_agent_auth_and_directory_operations() {
    let auth = AuthStore::for_test("password").await;
    auth.seed_test_workspace("default").await;
    auth.seed_test_workspace("other").await;
    let enrollment = auth
        .create_machine_enrollment("default", "admin")
        .await
        .unwrap();
    let machine = auth.claim_machine_enrollment(&enrollment).await.unwrap();
    let credential = auth
        .create_agent_credential("default", &machine.server_id, "agent-a")
        .await
        .unwrap();
    auth.create_agent_credential("other", "foreign-machine", "foreign-agent")
        .await
        .unwrap();
    let state = state_with_managed_agent().await;
    let mut agent = state.resolve_agent("default", "agent-a").await.unwrap();
    agent.server_id = machine.server_id.clone();
    state.test_insert_agent(agent).await;
    let router = workload_tree_agent_router(state.clone(), auth.clone(), PolicyEngine::allow_all());
    let base = "/agent/workspaces/default/workload-tree";
    let folders = format!("{base}/folders");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {}", machine.machine_token).parse().unwrap(),
    );
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().unwrap());
    let get = Method::GET;
    // Partial/spoofed workload identities fail before the handler.
    for credential in [None, Some("wrong")] {
        let mut headers = headers.clone();
        if let Some(credential) = credential {
            headers.insert(
                treer_protocol::WORKLOAD_CREDENTIAL_HEADER,
                credential.parse().unwrap(),
            );
        }
        let (status, _) = tree_request(&router, &headers, get.clone(), base, json!(null)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    headers.insert(
        treer_protocol::WORKLOAD_CREDENTIAL_HEADER,
        credential.parse().unwrap(),
    );
    let tree = |method: Method, path: String, body: Value| {
        let (router, headers) = (router.clone(), headers.clone());
        async move { tree_request(&router, &headers, method, &path, body).await }
    };
    let other = "/agent/workspaces/other/workload-tree".to_string();
    assert_eq!(
        tree(get.clone(), other, json!(null)).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, folder) = tree(Method::POST, folders.clone(), json!({"name":"research"})).await;
    assert_eq!(status, StatusCode::OK);
    let id = folder["folder"]["folder_id"].as_str().unwrap().to_string();
    let duplicate = tree(Method::POST, folders.clone(), json!({"name":"research"})).await;
    assert_eq!(duplicate.1["error"]["code"], "folder_exists");
    let placement = |agent: &str| format!("{base}/placements/{agent}");
    let moved = tree(
        Method::PUT,
        placement("agent-a"),
        json!({"folder_id":id,"position":99}),
    )
    .await;
    assert_eq!(moved.0, StatusCode::OK);
    assert_eq!(moved.1["placement"]["agent_id"], "agent-a");
    // Another workspace's workload cannot be placed here.
    let foreign = tree(
        Method::PUT,
        placement("foreign-agent"),
        json!({"folder_id":id,"position":0}),
    )
    .await;
    assert_eq!(foreign.0, StatusCode::BAD_REQUEST);
    let folder = format!("{base}/folders/{id}");
    let empty_only = format!("{folder}?require_empty=true");
    let not_empty = tree(Method::DELETE, empty_only.clone(), json!(null)).await;
    assert_eq!(not_empty.1["error"]["code"], "folder_not_empty");
    let renamed = tree(
        Method::PATCH,
        folder,
        json!({"name":"science","parent_id":null}),
    )
    .await;
    assert_eq!(renamed.0, StatusCode::OK);
    let snapshot = tree(get.clone(), base.to_string(), json!(null)).await.1;
    assert_eq!(snapshot["folders"][0]["name"], "science");
    assert_eq!(snapshot["placements"][0]["folder_id"], id);
    let unplaced = tree(
        Method::PUT,
        placement("agent-a"),
        json!({"folder_id":null,"position":0}),
    )
    .await;
    assert_eq!(unplaced.0, StatusCode::OK);
    assert_eq!(
        tree(Method::DELETE, empty_only, json!(null)).await.0,
        StatusCode::OK
    );

    // Tree authorization is independent of workload control or discovery.
    let policy = deny_policy(&auth, 0, &["agent.tree.read", "agent.tree.manage"]).await;
    let denied = workload_tree_agent_router(state, auth, policy);
    for (method, path) in [(get, base.to_string()), (Method::POST, folders)] {
        let body = json!({"name":"denied"});
        let (status, _) = tree_request(&denied, &headers, method, &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn public_workspace_snapshots_include_app_workloads() {
    let state = state_with_managed_agent().await;
    let mut app = state.resolve_agent("default", "agent-a").await.unwrap();
    app.agent_id = "app_stable".into();
    app.kind = "app".into();
    app.name = "Docs".into();
    state.test_insert_agent(app).await;
    let snapshot = state.snapshot("default").await.unwrap();
    assert_eq!(snapshot.agents.len(), 3);
    assert!(snapshot
        .agents
        .iter()
        .any(|agent| agent.agent_id == "app_stable"));
}

async fn app_lifecycle_fixture() -> (
    AuthStore,
    AppState,
    AppDeployment,
    tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) {
    app_lifecycle_fixture_with_update(false).await
}

async fn app_lifecycle_fixture_with_update(
    update: bool,
) -> (
    AuthStore,
    AppState,
    AppDeployment,
    tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let app = auth
        .create_app_deployment(
            "default",
            "owner",
            "machine-a".into(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".into()),
                name: "Docs".into(),
                command: "python3".into(),
                args: vec!["-m".into(), "http.server".into()],
                cwd: ".".into(),
                port: 8080,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(app.runtime_agent_id.as_deref(), Some(app.app_id.as_str()));
    let server = state_with_managed_agent()
        .await
        .resolve_server("default", "machine-a")
        .await
        .unwrap();
    let state = AppState::new();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "test-controller".into(),
            if update {
                vec![
                    treer_protocol::CAPABILITY_AGENT_LIFECYCLE.into(),
                    treer_protocol::CAPABILITY_APP_UPDATE.into(),
                ]
            } else {
                vec![treer_protocol::CAPABILITY_AGENT_LIFECYCLE.into()]
            },
            tx,
        )
        .await
        .unwrap();
    (auth, state, app, rx)
}

#[tokio::test]
async fn app_update_restore_validation_returns_current_spec_only_for_valid_machine_identity() {
    let (auth, _state, app, _rx) = app_lifecycle_fixture_with_update(true).await;
    auth.rotate_agent_credential("default", "machine-a", &app.app_id)
        .await
        .unwrap();
    let request = app_update_patch().apply(&app).unwrap();
    let guard = auth
        .workload_lifecycle_guard("default", &app.app_id)
        .await
        .unwrap();
    auth.update_app_startup("default", &app.app_id, &request, "owner")
        .await
        .unwrap();
    guard.commit().await.unwrap();
    let validate = |machine: &str| {
        agents::validate_agent_startup(
            Extension(auth.clone()),
            Extension(MachineSession {
                server_id: Some(machine.into()),
                workspace_id: Some("default".into()),
            }),
            Path(("default".into(), "machine-a".into())),
            Json(treer_protocol::ValidateAgentStartupRequest {
                agent_ids: vec![app.app_id.clone()],
            }),
        )
    };
    let response = validate("machine-a").await.unwrap().0;
    assert_eq!(response.app_startups.len(), 1);
    assert_eq!(response.app_startups[0].revision, 1);
    assert_eq!(response.app_startups[0].request, request);
    assert!(validate("another-machine").await.is_err());
    sqlx::query("UPDATE agent_credentials SET revoked_at=$1 WHERE agent_id=$2")
        .bind(Utc::now().to_rfc3339())
        .bind(&app.app_id)
        .execute(&auth.pool())
        .await
        .unwrap();
    let revoked = validate("machine-a").await.unwrap().0;
    assert!(
        revoked.app_startups.is_empty(),
        "revoked identities must not be restarted by spec reconciliation"
    );
    assert!(revoked.active_agent_ids.is_empty());
    assert!(
        revoked.deleted_agent_ids.is_empty(),
        "revocation alone is not a deletion confirmation"
    );
    auth.delete_agent("default", &app.app_id).await.unwrap();
    let deleted = agents::validate_agent_startup(
        Extension(auth.clone()),
        Extension(MachineSession {
            server_id: Some("machine-a".into()),
            workspace_id: Some("default".into()),
        }),
        Path(("default".into(), "machine-a".into())),
        Json(treer_protocol::ValidateAgentStartupRequest {
            agent_ids: vec![app.app_id.clone(), "ag_unrelated".into()],
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(deleted.deleted_agent_ids, vec![app.app_id.clone()]);
    assert!(deleted.active_agent_ids.is_empty());
}

fn app_update_patch() -> treer_protocol::UpdateAppDeploymentRequest {
    treer_protocol::UpdateAppDeploymentRequest {
        cwd: Some("releases/v2".into()),
        command: Some("node".into()),
        args: Some(vec!["entry.mjs".into()]),
    }
}

async fn invoke_app_update(
    auth: AuthStore,
    state: AppState,
    app: &AppDeployment,
    policy: PolicyEngine,
) -> Result<Json<Value>, ApiFailure> {
    apps::update_app_deployment(
        State(state),
        Extension(auth),
        Extension(test_ingress_config()),
        Extension(policy),
        None,
        Some(Extension(MachineSession {
            server_id: Some(app.server_id.clone()),
            workspace_id: Some(app.workspace_id.clone()),
        })),
        HeaderMap::new(),
        Path((app.workspace_id.clone(), app.app_id.clone())),
        Json(app_update_patch()),
    )
    .await
}

#[tokio::test]
async fn app_update_policy_denies_before_preflight_or_persistence() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture_with_update(true).await;
    for (index, denied) in [
        ACTION_AGENT_STARTUP_MANAGE,
        ACTION_AGENT_STOP,
        ACTION_AGENT_CREATE,
    ]
    .into_iter()
    .enumerate()
    {
        let policy = deny_policy(&auth, index as u64, &[denied]).await;
        let error = invoke_app_update(auth.clone(), state.clone(), &app, policy)
            .await
            .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            auth.resolve_app_deployment("default", &app.app_id)
                .await
                .unwrap()
                .startup_revision,
            0
        );
    }
}

pub(super) async fn next_lifecycle_command(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) -> treer_protocol::CommandEnvelope {
    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timely lifecycle command")
        .expect("connected Controller");
    let SocketFrame::Text(text) = frame else {
        panic!("expected command frame")
    };
    let ProxyMessage::Command { envelope } = serde_json::from_str(&text).unwrap() else {
        panic!("expected command")
    };
    envelope
}

#[tokio::test]
async fn app_lifecycle_start_and_restart_enforce_each_policy_action_before_dispatch() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
    for (index, (restart, denied)) in [
        (false, ACTION_AGENT_CREATE),
        (true, ACTION_AGENT_STOP),
        (true, ACTION_AGENT_CREATE),
    ]
    .into_iter()
    .enumerate()
    {
        let policy = Extension(deny_policy(&auth, index as u64, &[denied]).await);
        let path = Path(("default".into(), app.app_id.clone()));
        let (state, auth) = (State(state.clone()), Extension(auth.clone()));
        let result = if restart {
            workloads::restart_agent(
                state,
                auth,
                policy,
                None,
                machine_a(),
                HeaderMap::new(),
                path,
            )
            .await
        } else {
            workloads::start_agent(
                state,
                auth,
                policy,
                None,
                machine_a(),
                HeaderMap::new(),
                path,
            )
            .await
        };
        let response = result.expect_err("denied lifecycle action").into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "denied {denied}");
        assert!(
            rx.try_recv().is_err(),
            "authorization must precede all Controller commands"
        );
    }
}

pub(super) fn test_config() -> BootstrapConfig {
    BootstrapConfig::new(
        Url::parse("https://treer.example/").expect("valid URL"),
        PathBuf::from("dist"),
        Url::parse("https://github.example/releases/latest/download").expect("valid release URL"),
    )
}

fn test_browser_access() -> BrowserAccess {
    BrowserAccess::new(
        &Url::parse("https://app.treer.ai/").expect("app URL"),
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .expect("browser access")
}

#[test]
fn browser_tunnels_accept_app_and_proxy_origins_only() {
    let access = test_browser_access();
    for origin in ["https://app.treer.ai", "https://proxy.treer.ai"] {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
        access
            .validate_tunnel_if_present(&headers)
            .expect("trusted tunnel origin");
    }
    let mut denied = HeaderMap::new();
    denied.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://attacker.example"),
    );
    assert!(access.validate_tunnel_if_present(&denied).is_err());
}

pub(super) fn test_ingress_config() -> IngressConfig {
    IngressConfig::new(
        Some(Url::parse("https://apps.treer.ai/").expect("ingress URL")),
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        &Url::parse("https://app.treer.ai/").expect("app URL"),
    )
    .expect("ingress config")
}

#[test]
fn app_public_url_reports_the_managed_ingress_access() {
    let config = test_ingress_config();
    let now = Utc::now();
    let mut app = AppDeployment {
        startup_revision: 0,
        app_id: "app_1234".to_string(),
        workspace_id: "default".to_string(),
        name: "Public Docs".to_string(),
        server_id: "machine-a".to_string(),
        command: "python3".to_string(),
        args: vec![],
        cwd: ".".to_string(),
        port: 8080,
        hostname: String::new(),
        service_id: "svc_docs".to_string(),
        public_url: None,
        access: None,
        desired_state: AppDesiredState::Running,
        runtime_agent_id: None,
        restart_count: 0,
        status: AppDeploymentStatus::Pending,
        pid: None,
        exit_code: None,
        last_error: None,
        created_at: now,
        created_by: "agent-a".to_string(),
        updated_at: now,
        updated_by: "agent-a".to_string(),
    };
    let hostname = crate::auth::platform_service_hostname(
        &app.service_id,
        config.base_domain().expect("base domain"),
    )
    .expect("managed hostname");
    let ingress = ServiceIngress {
        ingress_id: "ing_docs".to_string(),
        workspace_id: "default".to_string(),
        service_id: Some(app.service_id.clone()),
        target_kind: treer_protocol::ServiceIngressTargetKind::Service,
        agent_id: None,
        hostname,
        access: ServiceIngressAccess::Public,
        enabled: true,
        created_at: now,
        created_by: "agent-a".to_string(),
        updated_at: now,
        updated_by: "agent-a".to_string(),
        limits: Default::default(),
    };

    attach_app_public_url(&config, &[ingress], &mut app);

    assert_eq!(app.access, Some(ServiceIngressAccess::Public));
    assert!(app
        .public_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://svc-")));
}

async fn admin_router(updater: crate::updater::UpdaterClient) -> Router {
    let auth = AuthStore::for_test("admin-password").await;
    router_with(
        auth,
        AppState::new(),
        CapabilityRollout::all_enabled(),
        updater,
    )
    .await
}

async fn admin_cookie(app: Router) -> (Router, HeaderValue) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/login")
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"password":"admin-password"}"#))
                .expect("login request"),
        )
        .await
        .expect("login response");
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("admin cookie")
        .to_str()
        .expect("cookie text");
    let token = set_cookie.split(';').next().expect("cookie pair");
    (app, HeaderValue::from_str(token).expect("cookie header"))
}

#[tokio::test]
async fn registered_cookie_routes_reject_browser_writes_before_handlers() {
    super::route_catalog::take_paths();
    let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
    let paths = super::route_catalog::take_paths();
    assert!(paths.len() > 100, "enumerate the actual complete router");
    // Prevent a future raw registration from silently escaping this inventory.
    for source in [
        include_str!("routes.rs"),
        include_str!("../admin.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap(),
    ] {
        assert!(
            !source.contains(".route("),
            "use registered_route for Core routes"
        );
    }
    for (path, origin_class) in paths {
        // Explicit exceptions, with the authentication boundary that replaces
        // browser-cookie authority. Unknown new routes must be classified.
        let exception = if path.starts_with("/agent/") {
            Some("machine/Agent credentials; no browser session selection")
        } else if path == "/api/machines/enroll" {
            Some("single-use enrollment credential")
        } else if path.starts_with("/.treer/identity/")
            || path.starts_with("/.treer/apps/identity/")
        {
            Some("verifies an explicit signed token, never a browser session")
        } else if path == "/api/apps/oauth/authorize" || path == "/.treer/ingress/authorize" {
            Some("top-level navigation; fixed ingress, audience-bound single-use expiring code")
        } else if path == "/api/apps/oauth/token" {
            Some("single-use authorization code plus S256 verifier")
        } else if path.starts_with("/api/apps/") {
            Some("service-audience App identity or attested Provider runtime; no cookie fallback")
        } else if path.ends_with("/callback") && path.starts_with("/api/auth/oauth/") {
            Some("external OAuth callback: single-use state and browser cookie binding")
        } else if matches!(
            path.as_str(),
            "/install.sh" | "/install.ps1" | "/api/health" | "/.well-known/jwks.json"
        ) || path.starts_with("/artifacts/")
        {
            Some("public read-only resource")
        } else {
            None
        };
        if exception.is_some() {
            continue;
        }
        assert!(
            path.starts_with("/api/workspaces")
                || path.starts_with("/api/organizations")
                || path.starts_with("/api/auth/")
                || path.starts_with("/api/admin/")
                || path.starts_with("/api/invitations/"),
            "unclassified route {path}"
        );
        let concrete = path
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    "fixture"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&concrete)
                    .header(header::ORIGIN, "https://proxy.treer.example")
                    .header(header::COOKIE, "treer_session=synthetic-user")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        match origin_class {
            crate::auth::CookieOriginClass::Embedded => assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{path}: Proxy origin must reach authentication, not CSRF denial"
            ),
            crate::auth::CookieOriginClass::Console => assert!(
                matches!(
                    response.status(),
                    StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
                ),
                "{path}: Proxy origin must not grant Core management authority"
            ),
        }
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            for origin in [None, Some("null"), Some("https://evil.example.test")] {
                let mut request = Request::builder()
                    .method(method)
                    .uri(&concrete)
                    .header(
                        header::COOKIE,
                        "treer_session=synthetic-user; treer_admin_session=synthetic-admin",
                    )
                    .header(header::CONTENT_TYPE, "text/plain");
                if let Some(origin) = origin {
                    request = request.header(header::ORIGIN, origin);
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::from("untrusted-body")).unwrap())
                    .await
                    .unwrap();
                assert!(
                    matches!(
                        response.status(),
                        StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
                    ),
                    "{method} {path}: {}",
                    response.status()
                );
            }
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(&concrete)
                        .header(header::ORIGIN, "https://app.treer.example")
                        .header("sec-fetch-site", "cross-site")
                        .header(header::CONTENT_TYPE, "text/plain")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                matches!(
                    response.status(),
                    StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
                ),
                "cross-site {method} {path}: {}",
                response.status()
            );
        }
    }
}

async fn spawn_updater_sidecar() -> Url {
    let app = Router::new()
        .route(
            "/v1/status",
            get(|| async {
                Json(serde_json::json!({"channel":"stable","services":[],"job":null}))
            }),
        )
        .route(
            "/v1/apply",
            post(|| async {
                (
                    StatusCode::ACCEPTED,
                    Json(serde_json::json!({
                        "channel": "stable",
                        "job": {"id": "job1", "state": "running", "error": null}
                    })),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind sidecar");
    let addr = listener.local_addr().expect("sidecar address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("sidecar server");
    });
    tokio::task::yield_now().await;
    Url::parse(&format!("http://{addr}/")).expect("sidecar URL")
}

#[test]
fn ingress_config_matches_one_wildcard_label_and_builds_urls() {
    let config = test_ingress_config();
    assert!(config.matches_hostname("demo.apps.treer.ai"));
    assert!(!config.matches_hostname("apps.treer.ai"));
    assert!(!config.matches_hostname("nested.demo.apps.treer.ai"));
    assert_eq!(
        config
            .url_for_hostname("demo.apps.treer.ai")
            .expect("ingress URL")
            .as_str(),
        "https://demo.apps.treer.ai/"
    );
}

#[tokio::test]
async fn trailing_slash_browser_tunnel_route_is_registered() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
    );
    for uri in [
        "/api/workspaces/default/virtual-hosts/self.test/proxy/",
        "/api/workspaces/default/apps/app-1/ui/",
        "/api/workspaces/default/apps/app-1/ui/app.js",
        "/api/workspaces/default/agents/agent-1/interface/ui",
        "/api/workspaces/default/agents/agent-1/interface/ui/",
        "/api/workspaces/default/agents/agent-1/interface/ui/ws",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-treer-interface-source", "controller-api")
                    .header("x-treer-agent-id", "agent-1")
                    .header("x-treer-interface-instance", "forged")
                    .body(Body::empty())
                    .expect("tunnel request"),
            )
            .await
            .expect("route response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json",
            "{uri}"
        );
        let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        let error: ApiError =
            serde_json::from_slice(&body).expect("JSON error, never launcher HTML");
        assert_eq!(error.error.code, "authentication_required", "{uri}");
    }
}

#[tokio::test]
async fn workspace_deletion_route_requires_auth_and_deletes_the_workspace() {
    let auth = AuthStore::for_test("admin-password").await;
    let (invite, _) = auth.create_personal_invitation().await.unwrap();
    let app = full_router(auth, AppState::new(), CapabilityRollout::all_enabled()).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/register")
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "invite": invite,
                        "email": "owner@example.com",
                        "preferred_name": "Owner",
                        "password": "password123",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = session_cookie(&response);
    let cookie = Some(cookie.as_str());
    let (status, organizations) = call(&app, Method::GET, "/api/organizations", cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    let organization_id = organizations["organizations"][0]["organization_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, created) = call(
        &app,
        Method::POST,
        "/api/workspaces",
        cookie,
        Some(json!({"organization_id": organization_id, "name": "Deletable"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let workspace = format!(
        "/api/workspaces/{}",
        created["workspace"]["workspace_id"].as_str().unwrap()
    );
    let (status, _) = call(&app, Method::DELETE, &workspace, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, deleted) = call(&app, Method::DELETE, &workspace, cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["name"], "Deletable");
    assert_eq!(deleted["organization_id"], organization_id);
    let list = format!("/api/workspaces?organization_id={organization_id}");
    let (status, listed) = call(&app, Method::GET, &list, cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed["workspaces"].as_array().unwrap().is_empty());
    let (status, _) = call(&app, Method::DELETE, &workspace, cookie, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

fn session_cookie(response: &axum::response::Response) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("treer_session="))
        .map(|value| value.split(';').next().expect("cookie pair").to_string())
        .expect("session cookie")
}

#[tokio::test]
async fn rollout_gates_core_message_traffic() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let enrollment = auth
        .create_machine_enrollment("default", "test")
        .await
        .expect("create rollout test enrollment");
    let machine = auth
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim rollout test machine");
    let authorization = HeaderValue::from_str(&format!("Bearer {}", machine.machine_token))
        .expect("machine authorization header");
    let app = full_router(auth, AppState::new(), CapabilityRollout::new(false)).await;

    for path in [
        "/agent/workspaces/default/messages",
        "/api/apps/svc_mail/messages",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(header::AUTHORIZATION, authorization.clone())
                    .body(Body::empty())
                    .expect("gated request"),
            )
            .await
            .expect("gated route response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read rollout error");
        let error: ApiError = serde_json::from_slice(&body).expect("decode rollout error");
        assert_eq!(error.error.code, "core_messages_disabled");
    }
}

#[tokio::test]
async fn machine_routes_expose_managed_apps_but_deny_direct_network_publication() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let enrollment = auth
        .create_machine_enrollment("default", "test")
        .await
        .expect("create App route enrollment");
    let machine = auth
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim App route machine");
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/agent/workspaces/default/apps")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", machine.machine_token),
                )
                .body(Body::empty())
                .expect("App list request"),
        )
        .await
        .expect("App list response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read App list response");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("decode App list"),
        json!({ "apps": [] })
    );

    let response = app.clone().oneshot(Request::builder().method(Method::POST)
        .uri("/agent/workspaces/default/services")
        .header(header::AUTHORIZATION, format!("Bearer {}",machine.machine_token))
        .header(header::CONTENT_TYPE,"application/json")
        .body(Body::from(serde_json::to_vec(&json!({"name":"preview","server_id":machine.server_id,"target_port":3000,"protocol":"http"})).unwrap())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&data).unwrap();
    let id = value["service"]["service_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/agent/workspaces/default/services")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", machine.machine_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"name":"wrong","server_id":"another-machine","target_port":3000}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/agent/workspaces/default/services/{id}/public-tcp"
                ))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", machine.machine_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"access":"public"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let data = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let error: ApiError = serde_json::from_slice(&data).unwrap();
    assert_eq!(error.error.code, "public_tcp_not_configured");
}

#[tokio::test]
async fn workspace_ingress_redirects_humans_to_proxy_authorization() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "private app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create service");
    let ingress = auth
        .create_service_ingress(
            "default",
            "test-user",
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id,
                slug: None,
                access: ServiceIngressAccess::Workspace,
            },
        )
        .await
        .expect("create ingress");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let request = Request::builder()
        .uri("/dashboard?tab=active")
        .header(header::HOST, &ingress.hostname)
        .body(Body::empty())
        .expect("ingress request");
    let response = proxy_service_ingress(
        State(AppState::new()),
        Extension(auth),
        Extension(PolicyEngine::new(
            crate::policy::PolicyDecision::Allow,
            vec![],
        )),
        Extension(test_ingress_config()),
        Extension(identity),
        request,
    )
    .await
    .expect("authorization redirect");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("redirect location")
        .to_str()
        .expect("location text");
    assert!(location.starts_with("https://proxy.treer.ai/.treer/ingress/authorize?"));
    assert!(location.contains("hostname=svc-"));
    assert!(location.contains("return_path=%2Fdashboard%3Ftab%3Dactive"));
}

#[test]
fn browser_access_accepts_only_the_configured_origin_when_present() {
    let browser = test_browser_access();
    assert!(browser.validate_if_present(&HeaderMap::new()).is_ok());

    let mut allowed = HeaderMap::new();
    allowed.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://app.treer.ai"),
    );
    assert!(browser.validate_if_present(&allowed).is_ok());

    let mut denied = HeaderMap::new();
    denied.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://other.treer.ai"),
    );
    let error = browser
        .validate_if_present(&denied)
        .expect_err("other browser origin must be denied");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.error.code, "browser_origin_denied");
}

/// The Console (app origin) and API (proxy origin) differ, so every custom-header
/// request preflights (#173). Only the configured Console origin is granted, with
/// credentials, including on error responses.
#[tokio::test]
async fn cors_grants_only_the_console_origin_with_credentials() {
    let auth = AuthStore::for_test("admin-password").await;
    let app = full_router(auth, AppState::new(), CapabilityRollout::all_enabled()).await;
    let send = |method: Method, uri: &str, origin: &str, preflight: Option<(&str, &str)>| {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::ORIGIN, origin);
        if let Some((method, headers)) = preflight {
            request = request
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, method)
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, headers);
        }
        app.clone().oneshot(request.body(Body::empty()).unwrap())
    };
    let header_text = |response: &axum::response::Response, name: header::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let console = "https://app.treer.ai";
    let upload = "/api/workspaces/default/machines/machine-a/uploads";
    let one = format!("{upload}/upl_0123456789abcdef0123456789abcdef");
    let parts = format!("{one}/parts?offset=0");
    for (uri, method, requested) in [
        (upload, "POST", "content-type,idempotency-key"),
        (parts.as_str(), "PUT", "content-type,x-treer-part-sha256"),
        (one.as_str(), "DELETE", "content-type"),
        (
            "/api/workspaces/default/machines/machine-a/files",
            "POST",
            "content-type",
        ),
        ("/api/auth/login", "POST", "content-type,x-treer-console"),
    ] {
        let response = send(Method::OPTIONS, uri, console, Some((method, requested)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{method} {uri}");
        assert_eq!(
            header_text(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
            console
        );
        assert_eq!(
            header_text(&response, header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
            "true"
        );
        let allowed = header_text(&response, header::ACCESS_CONTROL_ALLOW_HEADERS);
        for name in requested.split(',') {
            assert!(
                allowed.split(',').any(|entry| entry.trim() == name),
                "{method} {uri}: {name} missing from {allowed}"
            );
        }
        assert!(header_text(&response, header::ACCESS_CONTROL_ALLOW_METHODS)
            .contains(&method.to_ascii_lowercase()));
    }
    // Unlisted and native-client headers are never granted.
    let response = send(
        Method::OPTIONS,
        upload,
        console,
        Some(("POST", "idempotency-key,x-not-allowed,x-treer-client")),
    )
    .await
    .unwrap();
    let allowed = header_text(&response, header::ACCESS_CONTROL_ALLOW_HEADERS);
    assert!(!allowed.contains("x-not-allowed") && !allowed.contains("x-treer-client"));
    // A foreign origin is never mirrored.
    let response = send(
        Method::OPTIONS,
        upload,
        "https://evil.example",
        Some(("POST", "idempotency-key")),
    )
    .await
    .unwrap();
    assert_ne!(
        header_text(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "https://evil.example"
    );
    // Actual responses expose Retry-After; errors still carry CORS headers.
    let response = send(Method::GET, upload, console, None).await.unwrap();
    assert!(header_text(&response, header::ACCESS_CONTROL_EXPOSE_HEADERS).contains("retry-after"));
    let response = send(Method::GET, "/api/auth/me", console, None)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        header_text(&response, header::ACCESS_CONTROL_ALLOW_ORIGIN),
        console
    );
    assert_eq!(
        header_text(&response, header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
        "true"
    );
}

#[tokio::test]
async fn admin_update_requires_an_admin_session() {
    let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/update")
                .body(Body::empty())
                .expect("update request"),
        )
        .await
        .expect("update response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_update_forwards_to_the_sidecar() {
    let sidecar = spawn_updater_sidecar().await;
    let updater =
        crate::updater::UpdaterClient::new(sidecar, "secret".to_string()).expect("client");
    let app = admin_router(updater).await;
    let (app, cookie) = admin_cookie(app).await;
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/update")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .expect("status request"),
        )
        .await
        .expect("status response");
    assert_eq!(status.status(), StatusCode::OK);
    let apply = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/update")
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("apply request"),
        )
        .await
        .expect("apply response");
    assert_eq!(apply.status(), StatusCode::ACCEPTED);
}

#[test]
fn bootstrap_command_keeps_enrollment_tokens_out_of_the_url() {
    let config = test_config();
    let url = install_script_url(&config.public_url);
    assert_eq!(url.as_str(), "https://treer.example/install.sh");
    assert!(url.query().is_none());
}

#[test]
fn bootstrap_separates_public_installation_from_workspace_connection() {
    let config = test_config();
    let key = "enr_v1_64656661756c74_abc.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let (install, connect) = bootstrap_commands(&config.public_url, key);
    assert_eq!(
        install,
        "curl -fsSL 'https://treer.example/install.sh' | sh"
    );
    assert!(!install.contains("enr_"));
    assert!(!install.contains("connect"));
    assert_eq!(
        connect,
        format!("treer-agent-server connect --key '{key}' --proxy 'https://treer.example/'")
    );
    assert!(!connect.contains("install.sh"));
}

#[cfg(unix)]
#[test]
fn rendered_installer_has_valid_shell_syntax() {
    let config = test_config();
    let script = render_install_script(&config.public_url);
    let mut child = Command::new("sh")
        .arg("-n")
        .stdin(Stdio::piped())
        .spawn()
        .expect("start shell parser");
    child
        .stdin
        .take()
        .expect("shell stdin")
        .write_all(script.as_bytes())
        .expect("write installer");
    assert!(child.wait().expect("wait for shell parser").success());
}

#[test]
fn artifact_paths_reject_directory_traversal() {
    assert!(valid_artifact_component("linux-aarch64"));
    assert!(!valid_artifact_component("../linux-aarch64"));
    assert!(!valid_artifact_component("linux/aarch64"));
}

#[tokio::test]
async fn missing_local_artifacts_redirect_to_the_release() {
    let mut config = test_config();
    config.artifacts_dir = std::env::temp_dir().join(format!(
        "treer-missing-artifacts-{}",
        Uuid::new_v4().simple()
    ));
    let response = download_artifact(
        Extension(config),
        Path(("darwin-aarch64".to_string(), "treer".to_string())),
    )
    .await
    .unwrap_or_else(|_| panic!("missing artifact should redirect"));
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("https://github.example/releases/latest/download/treer-darwin-aarch64")
    );
}

#[test]
fn machine_principals_cannot_target_another_machine() {
    let subject = PolicySubject::Machine {
        server_id: "machine-a".to_string(),
    };
    assert!(require_machine_target(Some(&subject), "machine-a").is_ok());
    let error = require_machine_target(Some(&subject), "machine-b")
        .expect_err("cross-machine operation must require Agent identity");
    assert_eq!(error.error.code, "agent_identity_required");
}

#[test]
fn same_machine_agents_can_probe_sibling_services() {
    let timestamp = "2026-08-20T00:00:00Z".parse().expect("valid timestamp");
    let service = MachineService {
        service_id: "svc_ui".to_string(),
        workspace_id: "default".to_string(),
        name: "codex-ui".to_string(),
        server_id: "machine-a".to_string(),
        target_agent_id: Some("ag_ui".to_string()),
        primary: false,
        target_host: "127.0.0.1".to_string(),
        target_port: 4173,
        protocol: MachineServiceProtocol::Http,
        created_at: timestamp,
        created_by: "agent:ag_ui".to_string(),
        updated_at: timestamp,
        updated_by: "agent:ag_ui".to_string(),
    };
    let installer = PolicySubject::Agent {
        server_id: "machine-a".to_string(),
        agent_id: "ag_installer".to_string(),
    };
    let other_machine = PolicySubject::Agent {
        server_id: "machine-b".to_string(),
        agent_id: "ag_other".to_string(),
    };
    assert!(require_agent_can_probe_service(&installer, &service).is_ok());
    assert_eq!(
        require_agent_can_probe_service(&other_machine, &service)
            .expect_err("cross-machine probe")
            .error
            .code,
        "service_not_owned"
    );
}

#[tokio::test]
async fn agent_policy_subject_is_bound_to_authenticated_machine() {
    let state = state_with_managed_agent().await;
    let machine = MachineSession {
        server_id: Some("machine-a".to_string()),
        workspace_id: Some("default".to_string()),
    };
    let missing = agent_policy_subject(&state, &machine, &HeaderMap::new(), "default")
        .await
        .expect_err("agent identity is required");
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(missing.error.code, "invalid_agent_identity");

    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
    let subject = agent_policy_subject(&state, &machine, &headers, "default")
        .await
        .unwrap_or_else(|error| panic!("matching subject: {}", error.error.message));
    assert_eq!(
        subject,
        PolicySubject::Agent {
            server_id: "machine-a".to_string(),
            agent_id: "agent-a".to_string(),
        }
    );

    let error = agent_policy_subject(
        &state,
        &MachineSession {
            server_id: Some("machine-b".to_string()),
            workspace_id: Some("default".to_string()),
        },
        &headers,
        "default",
    )
    .await
    .expect_err("foreign machine must not claim the agent");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.error.code, "policy_subject_mismatch");
}

#[tokio::test]
async fn agent_identity_tokens_use_the_canonical_service_audience() {
    let state = state_with_managed_agent().await;
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test",
            CreateMachineServiceRequest {
                name: "api".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create service");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let machine = MachineSession {
        server_id: Some("machine-a".to_string()),
        workspace_id: Some("default".to_string()),
    };
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));

    let response = agent_issue_identity_token(
        State(state.clone()),
        Extension(WorkloadIdentityApi {
            auth: auth.clone(),
            policy: PolicyEngine::allow_all(),
            issuer: identity.clone(),
        }),
        Extension(machine),
        headers,
        Path("default".to_string()),
        Json(WorkloadIdentityTokenRequest {
            audience: "api".to_string(),
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("issue identity token: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read token response");
    let token: treer_protocol::WorkloadIdentityTokenResponse =
        serde_json::from_slice(&body).expect("decode token response");
    assert_eq!(token.audience, service.service_id);
    let verified = identity.verify(&token.access_token, &service.service_id);
    assert!(verified.active);
    let claims = verified.claims.expect("verified claims");
    assert_eq!(claims.sub, "agent-a");
    assert_eq!(claims.machine_id, "machine-a");

    let mut app_headers = HeaderMap::new();
    app_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.access_token))
            .expect("App authorization header"),
    );
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &app_headers, &service.service_id)
            .await
            .expect("authenticate Agent App Message identity");
    assert_eq!(workspace_id, "default");
    assert_eq!(principal.id, "agent-a");
    assert!(matches!(
        subject,
        PolicySubject::Agent { server_id, agent_id }
            if server_id == "machine-a" && agent_id == "agent-a"
    ));
}

#[tokio::test]
async fn message_api_pins_durable_policy_and_hides_recipient_resolution_details() {
    let state = state_with_managed_agent().await;
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    // A human sharing the reviewer Agent's display name makes "reviewer" ambiguous.
    sqlx::raw_sql("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('user-reviewer','reviewer@example.test',TRUE,'reviewer','unused','now'); INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','user-reviewer','member','now')")
        .execute(&auth.pool()).await.unwrap();
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let policy_store = WorkspacePolicyStore::new(auth.pool());
    let deny_send = WorkspacePolicyDocument {
        schema_version: POLICY_SCHEMA_VERSION,
        defaults: BTreeMap::from([(ACTION_MESSAGE_SEND.to_string(), PolicyEffect::Deny)]),
        groups: BTreeMap::new(),
        rules: Vec::new(),
    };
    let actor = PolicyPrincipalRef {
        kind: PolicyPrincipalKind::Human,
        id: "policy-owner".to_string(),
    };
    let monitor = policy_store
        .replace(
            "default",
            0,
            PolicyMode::Monitor,
            deny_send.clone(),
            actor.clone(),
        )
        .await
        .unwrap();
    let secret_body = "api-policy-body-must-not-enter-metadata";
    let send = |policy: PolicyEngine, recipient: &str, key: &str, body: &str| {
        send_core_message(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy),
            Extension(messages.clone()),
            machine_a().unwrap(),
            Path("default".to_string()),
            agent_a_headers(),
            Json(SendMessageRequest {
                sender_endpoint: None,
                recipients: vec![recipient.to_string()],
                context_ids: Vec::new(),
                body: body.to_string(),
                expires_at: None,
                idempotency_key: Some(key.to_string()),
                correlation_id: None,
                trace_id: None,
                external_source: None,
            }),
        )
    };
    let durable = PolicyEngine::durable(policy_store.clone());
    let sent = send(durable.clone(), "agent-b", "api-monitor-send", secret_body)
        .await
        .expect("monitor policy must observe rather than deny");
    let envelope: Value = sqlx::query_scalar(
        "SELECT envelope FROM core_message_outbox WHERE action = 'message.created' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(envelope["resource"]["id"], sent.0.message.message_id);
    assert_eq!(envelope["workspace_revision"], monitor.revision);
    assert!(!envelope.to_string().contains(secret_body));
    let audit: Vec<String> =
        sqlx::query_scalar("SELECT payload::text FROM organization_audit_events")
            .fetch_all(&auth.pool())
            .await
            .unwrap();
    assert!(audit.iter().all(|payload| !payload.contains(secret_body)));

    let allow = PolicyEngine::allow_all;
    let nonexistent = send(allow(), "missing-recipient", "api-missing", "x")
        .await
        .unwrap_err();
    assert_eq!(nonexistent.error.code, "agent_not_in_folder");
    let duplicate = send(allow(), "reviewer", "api-duplicate", "x")
        .await
        .unwrap_err();
    policy_store
        .replace(
            "default",
            monitor.revision,
            PolicyMode::Enforce,
            deny_send,
            actor,
        )
        .await
        .unwrap();
    let enforced = PolicyEngine::durable(policy_store);
    let hidden = send(enforced, "agent-b", "api-hidden", "x")
        .await
        .unwrap_err();
    // Ambiguity and Policy denial are indistinguishable to the sender.
    for error in [&duplicate, &hidden] {
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.error.code, "message_recipient_unavailable");
    }
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM core_messages")
        .fetch_one(&auth.pool())
        .await
        .unwrap();
    assert_eq!(stored, 1, "all denied sends must be atomic");
}

#[tokio::test]
async fn browser_tunnel_rejects_tcp_services_before_opening_a_stream() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "database".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 5432,
                protocol: treer_protocol::MachineServiceProtocol::Tcp,
            },
        )
        .await
        .expect("create machine service");
    auth.create_virtual_network_host(
        "default",
        "test-user",
        CreateVirtualNetworkHostRequest {
            hostname: "database.internal".to_string(),
            service_id: service.service_id,
        },
    )
    .await
    .expect("create virtual host");

    let error = proxy_virtual_network_host(
        AppState::new(),
        auth,
        "default".to_string(),
        "database.internal".to_string(),
        String::new(),
        Request::new(Body::empty()),
    )
    .await
    .expect_err("TCP services must not enter the HTTP tunnel");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.error.code, "service_protocol_mismatch");
}

#[tokio::test]
async fn browser_tunnel_forwards_http_without_leaking_gateway_credentials() {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
        capabilities: vec![],
    };
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");

    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create machine service");
    auth.create_virtual_network_host(
        "default",
        "test-user",
        CreateVirtualNetworkHostRequest {
            hostname: "app.internal".to_string(),
            service_id: service.service_id,
        },
    )
    .await
    .expect("create virtual host");

    let controller_state = state.clone();
    let controller = tokio::spawn(async move {
        let open = match server_rx.recv().await.expect("network open") {
            SocketFrame::Binary(encoded) => {
                treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
            }
            _ => panic!("expected network open"),
        };
        assert_eq!(open.kind, treer_protocol::NetworkBinaryKind::Open);
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::Opened,
                    stream_id: open.stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("open stream");

        let request = loop {
            let frame = match server_rx.recv().await.expect("HTTP request frame") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                }
                _ => continue,
            };
            if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                break String::from_utf8(frame.payload).expect("HTTP request text");
            }
        };
        assert!(request.starts_with("GET /status?full=1 HTTP/1.1\r\n"));
        assert!(request.contains("host: app.internal\r\n"));
        assert!(!request.to_ascii_lowercase().contains("cookie:"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));

        controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\nSet-Cookie: internal=secret\r\n\r\nhello".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::HalfClose,
                    stream_id: open.stream_id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("close response");
    });

    let request = Request::builder()
        .uri("/ignored?full=1")
        .header(header::COOKIE, "treer_session=secret")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .expect("browser request");
    let response = proxy_virtual_network_host(
        state,
        auth,
        "default".to_string(),
        "app.internal".to_string(),
        "status".to_string(),
        request,
    )
    .await
    .unwrap_or_else(|error| panic!("tunnel request: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .expect("read tunnel response");
    assert_eq!(&body[..], b"hello");
    controller.await.expect("join controller");
}

#[tokio::test]
async fn public_ingress_preserves_application_auth_and_strips_treer_headers() {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
        capabilities: vec![],
    };
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");

    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "public app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create machine service");
    let ingress = auth
        .create_service_ingress(
            "default",
            "test-user",
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id,
                slug: None,
                access: ServiceIngressAccess::Public,
            },
        )
        .await
        .expect("create ingress");

    let controller_state = state.clone();
    let controller = tokio::spawn(async move {
        let open = match server_rx.recv().await.expect("network open") {
            SocketFrame::Binary(encoded) => {
                treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
            }
            _ => panic!("expected network open"),
        };
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::Opened,
                    stream_id: open.stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("open stream");
        let request = loop {
            let frame = match server_rx.recv().await.expect("HTTP request frame") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                }
                _ => continue,
            };
            if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                break String::from_utf8(frame.payload).expect("HTTP request text");
            }
        };
        let lower = request.to_ascii_lowercase();
        assert!(request.starts_with("GET /api/items?limit=2 HTTP/1.1\r\n"));
        assert!(lower.contains("authorization: bearer application-token\r\n"));
        assert!(lower.contains("cookie: app_session=visible\r\n"));
        assert!(!lower.contains("treer_ingress"));
        assert!(!lower.contains("x-treer-spoofed"));
        assert!(lower.contains("host: svc-"));
        assert!(lower.contains(".apps.treer.ai\r\n"));
        controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\nSet-Cookie: app_session=updated; Path=/\r\n\r\nok".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::HalfClose,
                    stream_id: open.stream_id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("close response");
    });
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let request = Request::builder()
        .uri("/api/items?limit=2")
        .header(header::HOST, &ingress.hostname)
        .header(header::AUTHORIZATION, "Bearer application-token")
        .header(
            header::COOKIE,
            "__Host-treer_ingress=private; app_session=visible",
        )
        .header("x-treer-spoofed", "false")
        .body(Body::empty())
        .expect("ingress request");
    let response = proxy_service_ingress(
        State(state),
        Extension(auth),
        Extension(PolicyEngine::new(
            crate::policy::PolicyDecision::Allow,
            vec![],
        )),
        Extension(test_ingress_config()),
        Extension(identity),
        request,
    )
    .await
    .unwrap_or_else(|error| panic!("ingress request: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::SET_COOKIE),
        Some(&HeaderValue::from_static("app_session=updated; Path=/"))
    );
    controller.await.expect("join controller");
}

#[tokio::test]
async fn app_email_requires_owned_managed_app_identity_and_policy() {
    let (auth, _state, app, _rx) = app_lifecycle_fixture().await;
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://treer.example/").unwrap())
        .await
        .unwrap();
    let token = identity
        .issue(
            "default",
            "machine-a",
            app.runtime_agent_id.as_deref().unwrap(),
            &app.service_id,
        )
        .unwrap();
    let request = treer_protocol::SendAppEmailRequest {
        to: Some("person@example.test".into()),
        subject: "test".into(),
        text: "private".into(),
        idempotency_key: "k".into(),
        to_member: None,
    };
    let mut headers = HeaderMap::new();
    assert_eq!(
        send_app_email(
            Extension(auth.clone()),
            Extension(identity.clone()),
            Extension(PolicyEngine::allow_all()),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(request.clone())
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::UNAUTHORIZED
    );
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.access_token)).unwrap(),
    );
    // A legitimate Managed App with permission reaches sender configuration
    // even for a non-member recipient. Missing credentials above still fail.
    assert_eq!(
        send_app_email(
            Extension(auth.clone()),
            Extension(identity.clone()),
            Extension(PolicyEngine::allow_all()),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(request.clone())
        )
        .await
        .unwrap_err()
        .error
        .code,
        "email_not_configured"
    );
    let policy_store = WorkspacePolicyStore::new(auth.pool());
    policy_store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([("email.send".into(), PolicyEffect::Deny)]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        send_app_email(
            Extension(auth.clone()),
            Extension(identity.clone()),
            Extension(PolicyEngine::durable(policy_store)),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(request.clone())
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::FORBIDDEN
    );
    let wrong = identity
        .issue("default", "machine-a", "ordinary-agent", &app.service_id)
        .unwrap();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", wrong.access_token)).unwrap(),
    );
    assert_eq!(
        send_app_email(
            Extension(auth.clone()),
            Extension(identity.clone()),
            Extension(PolicyEngine::allow_all()),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(request.clone())
        )
        .await
        .unwrap_err()
        .error
        .code,
        "email_app_required"
    );
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.access_token)).unwrap(),
    );
    assert_eq!(
        send_app_email(
            Extension(auth.clone()),
            Extension(identity.clone()),
            Extension(PolicyEngine::allow_all()),
            Path("svc_wrong".into()),
            headers,
            Json(request.clone())
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::UNAUTHORIZED
    );
    // The Controller-relayed route (`treer email send`) shares the same checks:
    // only the App runtime on its own machine reaches the sender.
    let runtime = app.runtime_agent_id.clone().unwrap();
    for (server, agent, code) in [
        ("machine-a", runtime.as_str(), "email_not_configured"),
        ("machine-b", runtime.as_str(), "email_app_required"),
        ("machine-a", "ordinary-agent", "email_app_required"),
    ] {
        assert_eq!(
            super::email::relayed_email_code(&auth, server, agent, &request).await,
            code
        );
    }
    // Status lookup uses the same App-runtime authentication and ownership.
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.access_token)).unwrap(),
    );
    let missing = app_email_status(
        Extension(auth.clone()),
        Extension(identity.clone()),
        Path((app.service_id.clone(), "never-used".into())),
        headers,
    )
    .await
    .unwrap_err();
    assert_eq!(
        (missing.status, missing.error.code.as_str()),
        (StatusCode::NOT_FOUND, "email_not_found")
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", wrong.access_token)).unwrap(),
    );
    assert_eq!(
        app_email_status(
            Extension(auth),
            Extension(identity),
            Path((app.service_id.clone(), "never-used".into())),
            headers,
        )
        .await
        .unwrap_err()
        .error
        .code,
        "email_app_required"
    );
}

/// Keys a streamed `AgentInfo` may carry (#141): the member-visible flat
/// inventory. `path` is tolerated only as null; folders, placements and pins
/// never appear on the stream.
// The browser-visible Agent inventory (#153): `pid` is deliberately absent;
// bearer, Agent and machine credentials receive the full AgentInfo.
const STREAMED_AGENT_KEYS: [&str; 16] = [
    "path",
    "agent_id",
    "workspace_id",
    "server_id",
    "kind",
    "name",
    "cwd",
    "status",
    "run_generation",
    "started_at",
    "updated_at",
    "exited_at",
    "exit_code",
    "output_revision",
    "interface",
    "interface_port",
];
const STREAMED_SERVER_KEYS: [&str; 14] = [
    "server_id",
    "workspace_id",
    "name",
    "hostname",
    "root",
    "controller_build",
    "host_build",
    "supervision",
    "labels",
    "available_agents",
    "status",
    "connected_at",
    "last_seen_at",
    // Allow-listed live Controller capabilities (machine.upload,
    // machine.upload.v3) so the Console can pick the upload route (#173).
    "capabilities",
];
const STREAM_DENIED_KEYS: [&str; 7] = [
    "folder",
    "folder_id",
    "folders",
    "placement",
    "placements",
    "pins",
    "pinned_by",
];

fn assert_no_denied_keys(value: &Value, context: &str) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                assert!(
                    !STREAM_DENIED_KEYS.contains(&key.as_str()),
                    "{context}: Tree data key {key} on the stream"
                );
                if key == "path" {
                    assert!(inner.is_null(), "{context}: a path reached the stream");
                }
                assert_no_denied_keys(inner, context);
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| assert_no_denied_keys(item, context)),
        _ => {}
    }
}

fn assert_streamed_agent(agent: &Value, context: &str) {
    let keys = agent
        .as_object()
        .unwrap_or_else(|| panic!("{context}: agent object"));
    for key in keys.keys() {
        assert!(
            STREAMED_AGENT_KEYS.contains(&key.as_str()),
            "{context}: unexpected AgentInfo field {key} on the stream; extend the documented inventory or redact it"
        );
    }
    assert_no_denied_keys(agent, context);
}

/// Every workspace event kind carries only the documented member-visible
/// inventory: flat agent facts (including cwd and pid), machine facts, IDs.
pub(super) fn assert_stream_event_shape(event: &WorkspaceEvent) {
    let context = format!("event {}", event.event);
    assert_no_denied_keys(&event.data, &context);
    match event.event.as_str() {
        "workspace.snapshot" => {
            let mut keys: Vec<&str> = event
                .data
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["agents", "revision", "servers", "workspace"]);
            for agent in event.data["agents"].as_array().unwrap() {
                assert_streamed_agent(agent, &context);
            }
            for server in event.data["servers"].as_array().unwrap() {
                for key in server.as_object().unwrap().keys() {
                    assert!(
                        STREAMED_SERVER_KEYS.contains(&key.as_str()),
                        "{context}: server field {key}"
                    );
                }
            }
        }
        kind if kind.starts_with("agent.") => assert_streamed_agent(&event.data, &context),
        kind if kind.starts_with("server.") => {
            for key in event.data.as_object().unwrap().keys() {
                assert!(
                    STREAMED_SERVER_KEYS.contains(&key.as_str()),
                    "{context}: server field {key}"
                );
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn workspace_event_payloads_carry_only_the_flat_inventory() {
    let state = state_with_managed_agent().await;
    let mut events = state.subscribe();
    let snapshot = state.snapshot("default").await.unwrap();
    let browser = super::inventory_view::InventoryView::Browser;
    assert_stream_event_shape(&super::inventory_view::event_for_view(
        browser,
        &WorkspaceEvent {
            revision: snapshot.revision,
            workspace_id: "default".into(),
            event: "workspace.snapshot".into(),
            data: serde_json::to_value(&snapshot).unwrap(),
        },
    ));
    assert!(snapshot.agents.iter().all(|agent| agent.path.is_none()));
    state
        .rename_agent("default", "agent-a", "renamed".into())
        .await
        .unwrap();
    state
        .rename_server("default", "machine-a", "machine-renamed".into())
        .await
        .unwrap();
    state
        .publish_workspace_signal("default", "workspace.pins.changed", |revision| {
            json!({"revision": revision, "kind": "agent", "id": "agent-a", "action": "pin"})
        })
        .await
        .unwrap();
    let mut kinds = Vec::new();
    for _ in 0..3 {
        let event = events.recv().await.unwrap();
        assert_stream_event_shape(&super::inventory_view::event_for_view(browser, &event));
        kinds.push(event.event);
    }
    assert_eq!(
        kinds,
        ["agent.renamed", "server.renamed", "workspace.pins.changed"]
    );
}

#[tokio::test]
async fn pid_is_absent_for_browser_sessions_and_kept_for_bearer_and_machine_callers() {
    // #153: the browser inventory (session cookie) never carries pid; a
    // bearer credential (human CLI token, machine) keeps it.
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('member','member@example.test','Member','fixture','now')")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','member','member','now')")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES('tok-member','member','2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')")
        .execute(&auth.pool()).await.unwrap();
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://treer.example/").unwrap())
        .await
        .unwrap();
    let state = state_with_managed_agent().await;
    // Give agent-a a pid through a fresh Controller connection and snapshot.
    let snapshot = state.snapshot("default").await.unwrap();
    let server = snapshot
        .servers
        .iter()
        .find(|server| server.server_id == "machine-a")
        .cloned()
        .unwrap();
    let mut agents = snapshot.agents.clone();
    for agent in &mut agents {
        agent.pid = Some(4242);
        agent.path = None;
    }
    let connection = Uuid::new_v4();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection, tx)
        .await
        .unwrap();
    state
        .apply_snapshot(
            connection,
            treer_protocol::AgentServerSnapshot { server, agents },
        )
        .await
        .unwrap();
    let app = router(
        state.clone(),
        test_config(),
        auth.clone(),
        PolicyEngine::allow_all(),
        identity,
        BrowserAccess::new(
            &Url::parse("https://app.treer.example/").unwrap(),
            &Url::parse("https://proxy.treer.example/").unwrap(),
        )
        .unwrap(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
    );
    let fetch = |uri: &'static str, cookie: bool| {
        let app = app.clone();
        async move {
            let mut request = Request::builder().uri(uri);
            request = if cookie {
                request.header(header::COOKIE, "treer_session=tok-member")
            } else {
                request.header(header::AUTHORIZATION, "Bearer tok-member")
            };
            let response = app
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri} cookie={cookie}");
            let body = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&body).unwrap()
        }
    };
    fn agents_of(value: &Value) -> Vec<&Value> {
        match value.get("agents") {
            Some(Value::Array(items)) => items.iter().collect(),
            _ => vec![value],
        }
    }
    for uri in [
        "/api/workspaces/default/snapshot",
        "/api/workspaces/default/agents",
        "/api/workspaces/default/agents/agent-a",
    ] {
        let browser = fetch(uri, true).await;
        for agent in agents_of(&browser) {
            assert!(
                agent.get("pid").is_none(),
                "{uri}: browser view carries pid: {agent}"
            );
            assert!(agent.get("cwd").is_some(), "{uri}: cwd stays visible");
        }
        let bearer = fetch(uri, false).await;
        for agent in agents_of(&bearer) {
            assert_eq!(agent["pid"], 4242, "{uri}: bearer keeps pid: {agent}");
        }
    }
    // An invalid Bearer is refused rather than falling back to the cookie.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/agents")
                .header(header::COOKIE, "treer_session=tok-member")
                .header(header::AUTHORIZATION, "Bearer bogus-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "an invalid Bearer never falls back to the cookie"
    );
}

async fn connect_event_stream(
    addr: std::net::SocketAddr,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    let request = Request::builder()
        .uri(format!("ws://{addr}/api/workspaces/default/events"))
        .header(header::HOST, addr.to_string())
        .header(header::ORIGIN, "https://app.treer.example")
        .header(header::COOKIE, format!("treer_session={token}"))
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(
            header::SEC_WEBSOCKET_KEY,
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (socket, response) = tokio_tungstenite::client_async(request, stream)
        .await
        .expect("websocket upgrade");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    socket
}

async fn next_stream_message(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) -> tokio_tungstenite::tungstenite::Message {
    tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("timely stream message")
        .expect("open stream")
        .expect("stream frame")
}

fn stream_event(message: &tokio_tungstenite::tungstenite::Message) -> WorkspaceEvent {
    let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
        panic!("expected a text event, got {message:?}");
    };
    serde_json::from_str(text).expect("workspace event")
}

fn close_code(message: &tokio_tungstenite::tungstenite::Message) -> u16 {
    let tokio_tungstenite::tungstenite::Message::Close(Some(frame)) = message else {
        panic!("expected a close frame, got {message:?}");
    };
    assert!(frame.reason.is_empty(), "close reason must stay empty");
    u16::from(frame.code)
}

/// #141: a subscriber keeps receiving only while its session and workspace
/// membership hold. Revocation takes effect before the next delivery after
/// the re-check window, or on the idle tick, with fixed close codes and no
/// detail. Tree read never gates the flat inventory, so a Tree-read-denied
/// member receives agent events while GET /workload-tree answers 403.
#[tokio::test]
async fn workspace_event_stream_closes_on_revoked_membership_and_ended_session() {
    // The shrunk windows are process-global; serialize against any other
    // stream test and restore the defaults when this test ends.
    static STREAM_WINDOWS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _windows = STREAM_WINDOWS.lock().await;
    struct RestoreWindows;
    impl Drop for RestoreWindows {
        fn drop(&mut self) {
            super::events::set_stream_revalidation_for_test(
                super::events::STREAM_REVALIDATE_AFTER,
                super::events::STREAM_IDLE_TICK,
            );
        }
    }
    let _restore = RestoreWindows;
    super::events::set_stream_revalidation_for_test(
        Duration::from_millis(200),
        Duration::from_millis(400),
    );
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('member','member@example.test','Member','fixture','now'),('other','other@example.test','Other','fixture','now')")
        .execute(&auth.pool()).await.unwrap();
    for user in ["member", "other"] {
        sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default',$1,'member','now')")
            .bind(user).execute(&auth.pool()).await.unwrap();
        sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES($1,$2,'2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')")
            .bind(format!("tok-{user}")).bind(user).execute(&auth.pool()).await.unwrap();
    }
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://treer.example/").unwrap())
        .await
        .unwrap();
    let state = state_with_managed_agent().await;
    // Policy denies everything, including agent.tree.read: the flat inventory
    // is membership-based and still flows; the Tree route refuses.
    let denied = deny_all();
    let app = router(
        state.clone(),
        test_config(),
        auth.clone(),
        denied,
        identity,
        // The cookie CSRF check and the browser-origin check must agree on
        // the Console origin, which the test auth store sets to app.treer.example.
        BrowserAccess::new(
            &Url::parse("https://app.treer.example/").unwrap(),
            &Url::parse("https://proxy.treer.example/").unwrap(),
        )
        .unwrap(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
    );
    let tree = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/workload-tree")
                .header(header::COOKIE, "treer_session=tok-member")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tree.status(), StatusCode::FORBIDDEN);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // A member sees the flat inventory and live agent events, no Tree data.
    let mut member = connect_event_stream(addr, "tok-member").await;
    let initial = stream_event(&next_stream_message(&mut member).await);
    assert_eq!(initial.event, "workspace.snapshot");
    assert_eq!(initial.workspace_id, "default");
    assert_stream_event_shape(&initial);
    assert_eq!(initial.data["agents"].as_array().unwrap().len(), 2);
    state
        .rename_agent("default", "agent-a", "renamed".into())
        .await
        .unwrap();
    let renamed = stream_event(&next_stream_message(&mut member).await);
    assert_eq!(renamed.event, "agent.renamed");
    assert_eq!(renamed.data["name"], "renamed");
    assert_stream_event_shape(&renamed);

    // Membership revoked: the next delivery after the window is a 4403 close
    // and the event itself never reaches the former member.
    sqlx::query("DELETE FROM organization_members WHERE user_id='member'")
        .execute(&auth.pool())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    state
        .publish_workspace_signal("default", "workspace.pins.changed", |revision| {
            json!({"revision": revision, "kind": "agent", "id": "agent-a", "action": "pin"})
        })
        .await
        .unwrap();
    assert_eq!(close_code(&next_stream_message(&mut member).await), 4403);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), member.next())
            .await
            .map(|frame| frame.is_none() || frame.is_some_and(|f| f.is_err()))
            .unwrap_or(false),
        "nothing follows the close frame"
    );

    // An ended session is noticed on the idle tick even without events.
    let mut other = connect_event_stream(addr, "tok-other").await;
    assert_eq!(
        stream_event(&next_stream_message(&mut other).await).event,
        "workspace.snapshot"
    );
    sqlx::query("DELETE FROM sessions WHERE token='tok-other'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert_eq!(close_code(&next_stream_message(&mut other).await), 4401);

    // A fresh connection with the ended session is refused at the upgrade.
    let request = Request::builder()
        .uri(format!("ws://{addr}/api/workspaces/default/events"))
        .header(header::HOST, addr.to_string())
        .header(header::ORIGIN, "https://app.treer.example")
        .header(header::COOKIE, "treer_session=tok-other")
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(
            header::SEC_WEBSOCKET_KEY,
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let refused = tokio_tungstenite::client_async(request, stream).await;
    assert!(
        matches!(refused, Err(tokio_tungstenite::tungstenite::Error::Http(ref response)) if response.status() == StatusCode::UNAUTHORIZED),
        "{refused:?}"
    );
}
