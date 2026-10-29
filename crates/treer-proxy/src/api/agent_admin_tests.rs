//! Agent administration through Policy and the Settings administration
//! routes, exercised through the complete Core router and middleware.
use super::*;
use std::collections::BTreeMap;
use tower::ServiceExt;
use treer_protocol::{
    PolicyEffect, PolicyMode, PolicyPrincipalKind, PolicyPrincipalRef, WorkspacePolicyDocument,
    POLICY_SCHEMA_VERSION, WORKLOAD_CREDENTIAL_HEADER,
};
use treer_proxy::policy_store::WorkspacePolicyStore;

const ORIGIN: &str = "https://app.treer.example";

async fn register(
    auth: &AuthStore,
    email: &str,
    name: &str,
    password: &str,
) -> Result<CurrentSession, auth::AuthFailure> {
    let (token, _) = auth.create_personal_invitation().await.unwrap();
    auth.register(Some(&token), email, name, password).await
}

struct Fixture {
    auth: AuthStore,
    router: Router,
    store: WorkspacePolicyStore,
    owner: CurrentSession,
    organization_id: String,
    agent_headers: HeaderMap,
    machine_headers: HeaderMap,
    server_id: String,
}

async fn fixture() -> Fixture {
    let auth = AuthStore::for_test("password").await;
    let owner = register(&auth, "owner@example.com", "Owner", "owner-password")
        .await
        .unwrap();
    let organization_id = auth.list_organizations(&owner.user_id).await.unwrap()[0]
        .organization_id
        .clone();
    let info = auth
        .create_workspace(&organization_id, "ws_a", "Alpha", &owner.user_id)
        .await
        .unwrap();
    // A second organization the Agent must never reach.
    let other = register(&auth, "other@example.com", "Other", "other-password")
        .await
        .unwrap();
    let other_org = auth.list_organizations(&other.user_id).await.unwrap()[0]
        .organization_id
        .clone();
    auth.create_workspace(&other_org, "ws_b", "Beta", &other.user_id)
        .await
        .unwrap();
    let enrollment = auth
        .create_machine_enrollment("ws_a", &owner.user_id)
        .await
        .unwrap();
    let machine = auth.claim_machine_enrollment(&enrollment).await.unwrap();
    let credential = auth
        .create_agent_credential("ws_a", &machine.server_id, "agent-a")
        .await
        .unwrap();
    sqlx::query("INSERT INTO agent_names(agent_id, workspace_id, name, updated_at) VALUES('agent-a','ws_a','Admin Bot','now')")
        .execute(&auth.pool())
        .await
        .unwrap();
    let state = AppState::new();
    state.create_workspace_info(info).await.unwrap();
    let store = WorkspacePolicyStore::new(auth.pool());
    let policy = PolicyEngine::durable(store.clone());
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://treer.example/").unwrap())
        .await
        .unwrap();
    let router = router(
        state,
        super::tests::test_config(),
        auth.clone(),
        policy,
        identity,
        BrowserAccess::new(
            &Url::parse("https://app.treer.ai/").unwrap(),
            &Url::parse("https://proxy.treer.ai/").unwrap(),
        )
        .unwrap(),
        super::tests::test_ingress_config(),
        messages,
        CapabilityRollout::new(true),
        crate::updater::UpdaterClient::disabled(),
    );
    let mut machine_headers = HeaderMap::new();
    machine_headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {}", machine.machine_token).parse().unwrap(),
    );
    let mut agent_headers = machine_headers.clone();
    agent_headers.insert(AGENT_ID_HEADER, "agent-a".parse().unwrap());
    agent_headers.insert(WORKLOAD_CREDENTIAL_HEADER, credential.parse().unwrap());
    Fixture {
        auth,
        router,
        store,
        owner,
        organization_id,
        agent_headers,
        machine_headers,
        server_id: machine.server_id,
    }
}

async fn send(
    router: &Router,
    method: Method,
    uri: &str,
    headers: &HeaderMap,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    for (key, value) in headers {
        request = request.header(key, value);
    }
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = router
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

fn human(session: &CurrentSession) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::ORIGIN, ORIGIN.parse().unwrap());
    headers.insert(
        header::COOKIE,
        format!("{}={}", auth::SESSION_COOKIE, session.token)
            .parse()
            .unwrap(),
    );
    headers
}

impl Fixture {
    async fn agent(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        send(
            &self.router,
            method,
            &format!("/agent/workspaces/ws_a/admin/{path}"),
            &self.agent_headers,
            body,
        )
        .await
    }

    async fn publish(&self, mode: PolicyMode, document: Value) {
        let current = self.store.get("ws_a").await.unwrap();
        self.store
            .replace(
                "ws_a",
                current.map_or(0, |policy| policy.revision),
                mode,
                serde_json::from_value::<WorkspacePolicyDocument>(document).unwrap(),
                PolicyPrincipalRef {
                    kind: PolicyPrincipalKind::Human,
                    id: "owner".into(),
                },
            )
            .await
            .unwrap();
        // The durable evaluator caches for five seconds; tests use fresh routers
        // only through the store, so wait out the cache deterministically.
        tokio::time::sleep(Duration::from_millis(5_100)).await;
    }

    async fn grant(&self, actions: &[&str]) {
        self.publish(
            PolicyMode::Enforce,
            json!({
                "schema_version": POLICY_SCHEMA_VERSION,
                "rules": [{
                    "id": "agent-admin", "priority": 10, "effect": "allow",
                    "subjects": [{"kind": "agent", "id": "agent-a"}],
                    "actions": actions, "resources": [{}]
                }]
            }),
        )
        .await;
    }
}

#[tokio::test]
async fn agent_admin_is_default_deny_until_an_explicit_rule_names_the_agent() {
    let fx = fixture().await;
    let denied = |(status, body): (StatusCode, Value)| {
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["code"], "policy_denied", "{body}");
    };
    // No Policy at all: the Core fallback allows ordinary actions, never admin.
    denied(
        fx.agent(Method::GET, "organization/invitations", None)
            .await,
    );
    denied(
        fx.agent(
            Method::POST,
            "organization/invitations",
            Some(json!({"role": "member"})),
        )
        .await,
    );
    // Monitor mode, no rules, and an action default of allow: still denied.
    fx.publish(
        PolicyMode::Monitor,
        json!({"schema_version": POLICY_SCHEMA_VERSION,
               "defaults": {"org.invite.manage": "allow"}, "rules": []}),
    )
    .await;
    denied(
        fx.agent(Method::GET, "organization/invitations", None)
            .await,
    );
    // An enforced allow rule that selects every Agent by kind does not name it.
    fx.publish(
        PolicyMode::Enforce,
        json!({"schema_version": POLICY_SCHEMA_VERSION, "rules": [{
            "id": "all-agents", "priority": 1, "effect": "allow",
            "subjects": [{"kind": "agent"}], "actions": ["org.invite.manage"], "resources": [{}]
        }]}),
    )
    .await;
    denied(
        fx.agent(Method::GET, "organization/invitations", None)
            .await,
    );
    // A machine credential without an Agent identity is never an admin subject.
    let (status, body) = send(
        &fx.router,
        Method::GET,
        "/agent/workspaces/ws_a/admin/organization/invitations",
        &fx.machine_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "agent_identity_required");

    // An explicit rule naming the Agent grants exactly the listed actions.
    fx.grant(&["org.invite.manage"]).await;
    let (status, created) = fx
        .agent(
            Method::POST,
            "organization/invitations",
            Some(json!({"role": "member", "expires_in_hours": 2})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["created_by"]["kind"], "agent");
    assert_eq!(created["created_by"]["name"], "Admin Bot");
    assert!(created["url"].as_str().unwrap().contains("invite="));
    denied(fx.agent(Method::GET, "organization/members", None).await);
    denied(
        fx.agent(Method::PATCH, "workspace", Some(json!({"name": "Renamed"})))
            .await,
    );
    denied(
        fx.agent(Method::GET, "workspace/policy-provider", None)
            .await,
    );
    denied(fx.agent(Method::POST, "workspace/bootstrap", None).await);

    // A named allow rule also grants in monitor mode; a higher deny wins.
    fx.publish(
        PolicyMode::Monitor,
        json!({"schema_version": POLICY_SCHEMA_VERSION, "rules": [
            {"id": "allow", "priority": 1, "effect": "allow",
             "subjects": [{"kind": "agent", "id": "agent-a"}],
             "actions": ["org.invite.manage", "audit.read"], "resources": [{}]},
            {"id": "deny", "priority": 5, "effect": "deny",
             "subjects": [{"kind": "agent", "id": "agent-a"}],
             "actions": ["audit.read"], "resources": [{}]}
        ]}),
    )
    .await;
    assert_eq!(
        fx.agent(Method::GET, "organization/invitations", None)
            .await
            .0,
        StatusCode::OK
    );
    denied(
        fx.agent(Method::GET, "organization/audit-events", None)
            .await,
    );
}

#[tokio::test]
async fn agent_admin_is_scoped_to_its_workspace_and_organization() {
    let fx = fixture().await;
    fx.grant(&[
        "org.invite.manage",
        "workspace.manage",
        "workspace.access.manage",
    ])
    .await;
    // Another workspace's admin routes are refused before any handler runs.
    let (status, _) = send(
        &fx.router,
        Method::GET,
        "/agent/workspaces/ws_b/admin/organization/invitations",
        &fx.agent_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Grants cannot reference another organization's workspace.
    let (status, body) = fx
        .agent(
            Method::POST,
            "organization/invitations",
            Some(json!({"role": "member", "workspace_ids": ["ws_b"]})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invitation_workspace_not_found");
    // Workspace listing covers only the Agent's organization.
    let (status, body) = fx.agent(Method::GET, "workspaces", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<_> = body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["workspace_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["ws_a".to_string()]);
    // Store-level defense in depth: an Agent actor cannot touch organization B.
    let other_org = fx.auth.workspace_organization("ws_b").await.unwrap();
    let actor = auth::AdminActor::Agent {
        agent_id: "agent-a".into(),
        workspace_id: "ws_a".into(),
    };
    let error = fx
        .auth
        .list_organization_invitations(&other_org, &actor)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "agent_scope_denied");
    let error = fx
        .auth
        .workspace_access_info_as("ws_b", &actor)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "agent_scope_denied");
    // Its own workspace access works and reports the Agent role.
    let (status, body) = fx.agent(Method::GET, "workspace/access", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["access"]["current_role"], "agent");
}

#[tokio::test]
async fn agents_cannot_remove_or_demote_the_last_owner_and_are_audited() {
    let fx = fixture().await;
    fx.grant(&["org.member.manage", "audit.read", "org.group.manage"])
        .await;
    let owner = format!("organization/members/{}", fx.owner.user_id);
    let (status, body) = fx
        .agent(Method::PATCH, &owner, Some(json!({"role": "member"})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "last_owner");
    let (status, body) = fx.agent(Method::DELETE, &owner, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "last_owner");

    // With a second owner, demotion is allowed (an owner could do it).
    let (token, _) = fx
        .auth
        .create_invitation(&fx.organization_id, &fx.owner.user_id)
        .await
        .unwrap();
    let second = register(&fx.auth, "second@example.com", "Second", "second-password")
        .await
        .unwrap();
    fx.auth
        .accept_invitation(&token, &second.user_id)
        .await
        .unwrap();
    let path = format!("organization/members/{}", second.user_id);
    assert_eq!(
        fx.agent(Method::PATCH, &path, Some(json!({"role": "owner"})))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        fx.agent(Method::PATCH, &owner, Some(json!({"role": "admin"})))
            .await
            .0,
        StatusCode::OK
    );
    let (status, body) = fx
        .agent(Method::PATCH, &path, Some(json!({"role": "member"})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "last_owner");
    let (status, group) = fx
        .agent(
            Method::POST,
            "organization/groups",
            Some(json!({"name": "ops"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{group}");

    let (status, audit) = fx
        .agent(Method::GET, "organization/audit-events?limit=100", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{audit}");
    let events = audit["events"].as_array().unwrap();
    let by_agent = |action: &str| {
        events
            .iter()
            .find(|event| event["action"] == action && event["actor_kind"] == "agent")
    };
    for action in ["member.role_updated", "group.created"] {
        let event = by_agent(action).unwrap_or_else(|| panic!("{action} by agent: {audit}"));
        assert_eq!(event["actor_id"], "agent-a", "{event}");
        assert_eq!(event["actor_name"], "Admin Bot", "{event}");
    }
}

#[tokio::test]
async fn human_invitation_routes_create_list_revoke_and_validate() {
    let fx = fixture().await;
    let owner = human(&fx.owner);
    let base = format!("/api/organizations/{}/invitations", fx.organization_id);
    let group = fx
        .auth
        .create_organization_group(&fx.organization_id, &fx.owner.user_id, "reviewers")
        .await
        .unwrap();
    let (status, created) = send(
        &fx.router,
        Method::POST,
        &base,
        &owner,
        Some(
            json!({"email": "New@Example.com", "role": "admin", "expires_in_hours": 24,
                    "group_ids": [group.group_id], "workspace_ids": ["ws_a"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["email"], "new@example.com");
    assert_eq!(created["role"], "admin");
    assert_eq!(created["email_queued"], false);
    assert_eq!(created["created_by"]["id"], fx.owner.user_id.as_str());
    let id = created["id"].as_str().unwrap().to_string();
    // Legacy Consoles post an empty object.
    let (status, legacy) = send(&fx.router, Method::POST, &base, &owner, Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(legacy["role"], "member");

    for invalid in [
        json!({"role": "member", "expires_in_hours": 0}),
        json!({"role": "member", "expires_in_hours": 721}),
        json!({"role": "member", "group_ids": ["grp_missing"]}),
        json!({"role": "member", "email": "not an email"}),
        json!({"role": "superuser"}),
    ] {
        let (status, body) = send(
            &fx.router,
            Method::POST,
            &base,
            &owner,
            Some(invalid.clone()),
        )
        .await;
        assert!(status.is_client_error(), "{invalid} -> {status} {body}");
    }

    let (status, list) = send(&fx.router, Method::GET, &base, &owner, None).await;
    assert_eq!(status, StatusCode::OK);
    let invitations = list["invitations"].as_array().unwrap();
    assert_eq!(invitations.len(), 2);
    let listed = invitations.iter().find(|i| i["id"] == id.as_str()).unwrap();
    assert_eq!(listed["status"], "pending");
    assert_eq!(listed["created_by"]["name"], "Owner");
    // The secret is never returned again.
    assert!(!list.to_string().contains("inv_"), "{list}");

    let (status, _) = send(
        &fx.router,
        Method::DELETE,
        &format!("{base}/{id}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = send(&fx.router, Method::GET, &base, &owner, None).await;
    assert!(list["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["id"] == id.as_str() && i["status"] == "revoked"));
    let token = Url::parse(created["url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "invite")
        .unwrap()
        .1
        .into_owned();
    let joiner = register(&fx.auth, "joiner@example.com", "Joiner", "joiner-password")
        .await
        .unwrap();
    assert!(fx
        .auth
        .accept_invitation(&token, &joiner.user_id)
        .await
        .is_err());

    // A member cannot manage invitations; an admin cannot invite an owner.
    let member_role = |role: &'static str| {
        let fx = &fx;
        async move {
            let (token, _) = fx
                .auth
                .create_invitation(&fx.organization_id, &fx.owner.user_id)
                .await
                .unwrap();
            let user = register(
                &fx.auth,
                &format!("{role}@example.com"),
                role,
                "user-password",
            )
            .await
            .unwrap();
            fx.auth
                .accept_invitation(&token, &user.user_id)
                .await
                .unwrap();
            if role != "member" {
                fx.auth
                    .update_member_role(&fx.organization_id, &fx.owner.user_id, &user.user_id, role)
                    .await
                    .unwrap();
            }
            user
        }
    };
    let member = member_role("member").await;
    let (status, _) = send(&fx.router, Method::GET, &base, &human(&member), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let admin = member_role("admin").await;
    let (status, body) = send(
        &fx.router,
        Method::POST,
        &base,
        &human(&admin),
        Some(json!({"role": "owner"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn accepting_an_invitation_applies_role_groups_and_workspace_grants() {
    let fx = fixture().await;
    let group = fx
        .auth
        .create_organization_group(&fx.organization_id, &fx.owner.user_id, "reviewers")
        .await
        .unwrap();
    fx.auth
        .update_workspace_access_mode("ws_a", &fx.owner.user_id, "restricted")
        .await
        .unwrap();
    let created = fx
        .auth
        .create_organization_invitation(
            &fx.organization_id,
            &auth::AdminActor::user(&fx.owner.user_id),
            treer_protocol::CreateInvitationRequest {
                email: None,
                role: treer_protocol::OrganizationRole::Admin,
                expires_in_hours: Some(1),
                group_ids: vec![group.group_id.clone()],
                workspace_ids: vec!["ws_a".into()],
            },
        )
        .await
        .unwrap();
    let token = created.url.split("invite=").nth(1).unwrap().to_string();
    let user = register(&fx.auth, "joiner@example.com", "Joiner", "joiner-password")
        .await
        .unwrap();
    let accepted = fx
        .auth
        .accept_invitation(&token, &user.user_id)
        .await
        .unwrap();
    assert_eq!(accepted.role, "admin");
    let groups = fx
        .auth
        .list_organization_groups(&fx.organization_id, &user.user_id)
        .await
        .unwrap();
    assert!(groups[0].member_ids.contains(&user.user_id));
    let access = fx
        .auth
        .workspace_access_info("ws_a", &fx.owner.user_id)
        .await
        .unwrap();
    assert!(access
        .members
        .iter()
        .any(|m| m.user_id == user.user_id && m.role == "member"));
    // Expired invitations are refused.
    sqlx::query("UPDATE invitations SET expires_at = now() - interval '1 minute', used_at = NULL WHERE invitation_id = $1")
        .bind(&created.id)
        .execute(&fx.auth.pool())
        .await
        .unwrap();
    let late = register(&fx.auth, "late@example.com", "Late", "late-password")
        .await
        .unwrap();
    assert!(fx
        .auth
        .accept_invitation(&token, &late.user_id)
        .await
        .is_err());
    let list = fx
        .auth
        .list_organization_invitations(
            &fx.organization_id,
            &auth::AdminActor::user(&fx.owner.user_id),
        )
        .await
        .unwrap();
    assert_eq!(list[0].status, treer_protocol::InvitationStatus::Expired);
}

#[tokio::test]
async fn organization_leave_transfer_and_delete() {
    let fx = fixture().await;
    let owner = human(&fx.owner);
    let org = format!("/api/organizations/{}", fx.organization_id);
    // The last owner cannot leave.
    let (status, body) = send(
        &fx.router,
        Method::POST,
        &format!("{org}/leave"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "last_owner");
    let (token, _) = fx
        .auth
        .create_invitation(&fx.organization_id, &fx.owner.user_id)
        .await
        .unwrap();
    let member = register(&fx.auth, "member@example.com", "Member", "member-password")
        .await
        .unwrap();
    fx.auth
        .accept_invitation(&token, &member.user_id)
        .await
        .unwrap();
    // Only an owner transfers; the target must be another member.
    let transfer = format!("{org}/transfer");
    let (status, _) = send(
        &fx.router,
        Method::POST,
        &transfer,
        &human(&member),
        Some(json!({"user_id": fx.owner.user_id})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &fx.router,
        Method::POST,
        &transfer,
        &owner,
        Some(json!({"user_id": "usr_missing"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &fx.router,
        Method::POST,
        &transfer,
        &owner,
        Some(json!({"user_id": member.user_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let members = fx
        .auth
        .list_members(&fx.organization_id, &fx.owner.user_id)
        .await
        .unwrap();
    let role = |id: &str| {
        members
            .iter()
            .find(|m| m.user_id == id)
            .unwrap()
            .role
            .clone()
    };
    assert_eq!(role(&member.user_id), "owner");
    assert_eq!(role(&fx.owner.user_id), "admin");
    // The former owner (now admin) may leave.
    let (status, _) = send(
        &fx.router,
        Method::POST,
        &format!("{org}/leave"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Delete: owner only, exact name, and only without workspaces.
    let new_owner = human(&member);
    let (status, body) = send(
        &fx.router,
        Method::DELETE,
        &org,
        &new_owner,
        Some(json!({"confirm_name": "wrong"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let name = fx
        .auth
        .list_organizations(&member.user_id)
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.organization_id == fx.organization_id)
        .unwrap()
        .name;
    let (status, body) = send(
        &fx.router,
        Method::DELETE,
        &org,
        &new_owner,
        Some(json!({"confirm_name": name})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "organization_has_workspaces");
    assert_eq!(body["workspace_count"], 1);
    let (status, body) = send(
        &fx.router,
        Method::DELETE,
        "/api/workspaces/ws_a",
        &new_owner,
        Some(json!({"confirm_name": "Alpha", "revoke_machines": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = send(
        &fx.router,
        Method::DELETE,
        &org,
        &new_owner,
        Some(json!({"confirm_name": name})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(fx
        .auth
        .list_organizations(&member.user_id)
        .await
        .unwrap()
        .iter()
        .all(|o| o.organization_id != fx.organization_id));
}

#[tokio::test]
async fn workspace_delete_revokes_machines_in_one_step_and_effective_access_explains_roles() {
    let fx = fixture().await;
    let owner = human(&fx.owner);
    let (status, effective) = send(
        &fx.router,
        Method::GET,
        "/api/workspaces/ws_a/access/effective",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{effective}");
    assert_eq!(effective["mode"], "organization");
    let me = &effective["members"][0];
    assert_eq!(me["user"]["id"], fx.owner.user_id.as_str());
    assert_eq!(me["role"], "owner");
    let kinds: Vec<_> = me["via"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["kind"].clone())
        .collect();
    assert!(kinds.contains(&json!("org_role")) && kinds.contains(&json!("direct")));

    // Default delete still refuses while machines remain.
    let (status, body) = send(
        &fx.router,
        Method::DELETE,
        "/api/workspaces/ws_a",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "workspace_has_machines");
    for invalid in [
        json!({"revoke_machines": true}),
        json!({"confirm_name": "Wrong", "revoke_machines": true}),
    ] {
        let (status, _) = send(
            &fx.router,
            Method::DELETE,
            "/api/workspaces/ws_a",
            &owner,
            Some(invalid),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, body) = send(
        &fx.router,
        Method::DELETE,
        "/api/workspaces/ws_a",
        &owner,
        Some(json!({"confirm_name": "Alpha", "revoke_machines": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["machine_count"], 1);
    let revoked: bool =
        sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM machines WHERE server_id = $1")
            .bind(&fx.server_id)
            .fetch_one(&fx.auth.pool())
            .await
            .unwrap();
    assert!(revoked);
    // The machine credential no longer authenticates.
    let (status, _) = send(
        &fx.router,
        Method::GET,
        "/agent/workspaces/ws_a/admin/workspaces",
        &fx.agent_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn account_sessions_and_password_change() {
    let fx = fixture().await;
    let second = fx
        .auth
        .login("owner@example.com", "owner-password")
        .await
        .unwrap();
    let headers = human(&fx.owner);
    let (status, list) = send(
        &fx.router,
        Method::GET,
        "/api/auth/sessions",
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let sessions = list["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
    assert!(!list.to_string().contains(&fx.owner.token));
    let other = sessions.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = send(
        &fx.router,
        Method::DELETE,
        &format!("/api/auth/sessions/{other}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(fx
        .auth
        .revalidate_session(&second.token)
        .await
        .unwrap()
        .is_none());
    let (status, _) = send(
        &fx.router,
        Method::DELETE,
        "/api/auth/sessions/ses_missing",
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let third = fx
        .auth
        .login("owner@example.com", "owner-password")
        .await
        .unwrap();
    for (body, expected) in [
        (
            json!({"current_password": "wrong-password", "new_password": "fresh-password"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"current_password": "owner-password", "new_password": "short"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, _) = send(
            &fx.router,
            Method::POST,
            "/api/auth/password",
            &headers,
            Some(body),
        )
        .await;
        assert_eq!(status, expected);
    }
    let (status, _) = send(
        &fx.router,
        Method::POST,
        "/api/auth/password",
        &headers,
        Some(json!({"current_password": "owner-password", "new_password": "fresh-password"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Other sessions end; the current one stays; the new password works.
    assert!(fx
        .auth
        .revalidate_session(&third.token)
        .await
        .unwrap()
        .is_none());
    assert!(fx
        .auth
        .revalidate_session(&fx.owner.token)
        .await
        .unwrap()
        .is_some());
    assert!(fx
        .auth
        .login("owner@example.com", "fresh-password")
        .await
        .is_ok());

    // Agents never reach account routes: workload headers are not a session.
    let (status, _) = send(
        &fx.router,
        Method::GET,
        "/api/auth/sessions",
        &fx.agent_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn agent_workspace_and_enrollment_routes_with_grants() {
    let fx = fixture().await;
    fx.grant(&[
        "workspace.manage",
        "machine.enroll",
        "workspace.access.manage",
    ])
    .await;
    let (status, body) = fx
        .agent(Method::POST, "workspaces", Some(json!({"name": "Gamma"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let created = body["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        fx.auth.workspace_organization(&created).await.unwrap(),
        fx.organization_id
    );
    let (status, body) = fx
        .agent(Method::PATCH, "workspace", Some(json!({"name": "Alpha 2"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = fx.agent(Method::POST, "workspace/bootstrap", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["enrollment_key"].as_str().is_some());
    let (status, body) = fx
        .agent(
            Method::PUT,
            &format!("workspace/access/users/{}", fx.owner.user_id),
            Some(json!({"role": "owner"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = fx
        .agent(Method::GET, "workspace/access/effective", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["members"][0]["role"], "owner");
    let creator: String =
        sqlx::query_scalar("SELECT created_by FROM workspaces WHERE workspace_id = $1")
            .bind(&created)
            .fetch_one(&fx.auth.pool())
            .await
            .unwrap();
    assert_eq!(creator, "agent:agent-a");
}

#[test]
fn admin_actions_are_listed_for_default_deny() {
    use crate::policy::ADMIN_ACTIONS;
    for action in [
        "org.manage",
        "org.member.manage",
        "org.invite.manage",
        "org.group.manage",
        "workspace.manage",
        "workspace.access.manage",
        "policy.provider.manage",
        "audit.read",
        "machine.enroll",
    ] {
        assert!(ADMIN_ACTIONS.contains(&action), "{action}");
    }
    let _ = (BTreeMap::<String, PolicyEffect>::new(), PolicyMode::Monitor);
}
