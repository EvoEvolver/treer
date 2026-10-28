//! Workspace-shared Overview pins (#140): Tree authorisation for reads and
//! writes, idempotency, the per-workspace limit, and the IDs-only live signal.
use super::*;
use crate::audit::{MutationActor, MUTATION_ACTOR};
use std::collections::BTreeMap;
use treer_protocol::{
    PolicyEffect, PolicyMode, PolicyPrincipalKind, PolicyPrincipalRef, WorkspacePinKind,
    WorkspacePinsResponse, WorkspacePolicyDocument, POLICY_SCHEMA_VERSION,
    WORKSPACE_PINS_CHANGED_EVENT, WORKSPACE_PINS_LIMIT,
};
use treer_proxy::policy_store::WorkspacePolicyStore;

const WS: &str = "default";

fn session(user: &str) -> CurrentSession {
    CurrentSession {
        token: format!("session-{user}"),
        user_id: user.into(),
        email: format!("{user}@example.test"),
        preferred_name: user.into(),
    }
}

async fn seeded() -> (AppState, AuthStore) {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace(WS).await;
    auth.seed_test_workspace("other").await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('creator','creator@example.test','Creator','fixture','now'),('viewer','viewer@example.test','Viewer','fixture','now'),('outsider','outsider@example.test','Outsider','fixture','now')")
        .execute(&auth.pool()).await.unwrap();
    for user in ["creator", "viewer"] {
        sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES($1,$2,'member','now')")
            .bind(format!("org_{WS}")).bind(user).execute(&auth.pool()).await.unwrap();
    }
    auth.create_named_agent_credential(WS, "machine-a", "agent-a", "agent-a", None)
        .await
        .unwrap();
    auth.create_named_agent_credential(WS, "machine-a", "agent-b", "reviewer", None)
        .await
        .unwrap();
    for (app, workspace) in [("app_one", WS), ("app_other", "other")] {
        sqlx::query("INSERT INTO machine_services(service_id, workspace_id, name, server_id, target_host, target_port, protocol, created_at, created_by, updated_at, updated_by) VALUES($1,$2,'app','machine-a','127.0.0.1',8787,'http','now','creator','now','creator')")
            .bind(format!("svc_{app}")).bind(workspace).execute(&auth.pool()).await.unwrap();
        sqlx::query("INSERT INTO app_deployments(app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, service_id, created_at, created_by, updated_at, updated_by) VALUES($1,$2,'app','machine-a','python3','[]'::jsonb,'.',8787,'app.internal',$3,'now','creator','now','creator')")
            .bind(app).bind(workspace).bind(format!("svc_{app}")).execute(&auth.pool()).await.unwrap();
    }
    let state = super::tests::state_with_managed_agent().await;
    (state, auth)
}

struct Pins {
    state: AppState,
    auth: AuthStore,
}

impl Pins {
    async fn list(
        &self,
        policy: &PolicyEngine,
        user: &str,
    ) -> Result<WorkspacePinsResponse, ApiFailure> {
        super::pins::list_workspace_pins(
            State(self.state.clone()),
            Extension(self.auth.clone()),
            Extension(policy.clone()),
            Extension(session(user)),
            HeaderMap::new(),
            Path(WS.into()),
        )
        .await
        .map(|json| json.0)
    }

    /// PUT (pin) or DELETE (unpin) with the mutation actor production middleware scopes.
    async fn write(
        &self,
        put: bool,
        policy: &PolicyEngine,
        user: &str,
        kind: &str,
        id: &str,
    ) -> Result<axum::response::Response, ApiFailure> {
        let args = || {
            (
                State(self.state.clone()),
                Extension(self.auth.clone()),
                Extension(policy.clone()),
                Extension(session(user)),
                HeaderMap::new(),
                Path((WS.to_string(), kind.to_string(), id.to_string())),
            )
        };
        let actor = MutationActor {
            kind: "user",
            id: Some(user.to_owned()),
        };
        let (a, b, c, d, e, f) = args();
        if put {
            MUTATION_ACTOR
                .scope(actor, super::pins::pin_workspace_item(a, b, c, d, e, f))
                .await
                .map(IntoResponse::into_response)
        } else {
            MUTATION_ACTOR
                .scope(actor, super::pins::unpin_workspace_item(a, b, c, d, e, f))
                .await
                .map(IntoResponse::into_response)
        }
    }

    async fn pin(
        &self,
        policy: &PolicyEngine,
        user: &str,
        kind: &str,
        id: &str,
    ) -> Result<Value, ApiFailure> {
        let response = self.write(true, policy, user, kind, id).await?;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        Ok(serde_json::from_slice(&bytes).unwrap())
    }

    async fn unpin(
        &self,
        policy: &PolicyEngine,
        user: &str,
        kind: &str,
        id: &str,
    ) -> Result<StatusCode, ApiFailure> {
        Ok(self.write(false, policy, user, kind, id).await?.status())
    }

    async fn audit_count(&self) -> usize {
        crate::audit::list(&self.auth.pool(), &format!("org_{WS}"), Some(WS), None, 500)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.action.starts_with("tree.pin."))
            .count()
    }
}

async fn next_pins_event(
    events: &mut tokio::sync::broadcast::Receiver<treer_protocol::WorkspaceEvent>,
) -> Option<treer_protocol::WorkspaceEvent> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(event)) if event.event == WORKSPACE_PINS_CHANGED_EVENT => return Some(event),
            Ok(Ok(_)) => continue,
            _ => return None,
        }
    }
}

async fn fixture() -> Pins {
    let (state, auth) = seeded().await;
    Pins { state, auth }
}

async fn folder(auth: &AuthStore, name: &str) -> String {
    auth.create_workload_folder(
        WS,
        treer_protocol::CreateWorkloadFolderRequest {
            name: name.into(),
            parent_id: None,
        },
    )
    .await
    .unwrap()
    .folder_id
}

#[tokio::test]
async fn pins_round_trip_is_idempotent_and_signals_ids_only() {
    let pins = fixture().await;
    let allow = PolicyEngine::allow_all();
    let mut events = pins.state.subscribe();
    let empty = pins.list(&allow, "creator").await.unwrap();
    assert!(empty.pins.is_empty() && empty.can_pin);
    assert_eq!(empty.limit, WORKSPACE_PINS_LIMIT);

    let folder = folder(&pins.auth, "research").await;
    let first = pins
        .pin(&allow, "creator", "agent", "agent-a")
        .await
        .unwrap();
    assert_eq!(first["kind"], "agent");
    assert_eq!(first["id"], "agent-a");
    assert_eq!(first["pinned_by"], "creator");
    assert!(first.get("name").is_none() && first.get("path").is_none());
    let event = next_pins_event(&mut events).await.expect("pinned signal");
    assert_eq!(
        event.data,
        json!({"action":"pinned","kind":"agent","id":"agent-a","revision":event.revision})
    );
    // Idempotent re-pin: same row, no second audit row or event.
    let again = pins
        .pin(&allow, "viewer", "agent", "agent-a")
        .await
        .unwrap();
    assert_eq!(again["pinned_by"], "creator");
    assert_eq!(pins.audit_count().await, 1);
    assert!(next_pins_event(&mut events).await.is_none());

    pins.pin(&allow, "viewer", "app", "app_one").await.unwrap();
    pins.pin(&allow, "creator", "folder", &folder)
        .await
        .unwrap();
    let listed = pins.list(&allow, "creator").await.unwrap();
    assert_eq!(
        listed
            .pins
            .iter()
            .map(|pin| (pin.kind, pin.id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (WorkspacePinKind::Folder, folder.as_str()),
            (WorkspacePinKind::App, "app_one"),
            (WorkspacePinKind::Agent, "agent-a"),
        ],
        "newest first"
    );
    let serialized = serde_json::to_string(&listed).unwrap();
    assert!(!serialized.contains("research") && !serialized.contains("reviewer"));

    // Unknown item or another workspace's App: uniform 404; malformed: 400.
    for (kind, id) in [
        ("agent", "ag_missing"),
        ("app", "app_other"),
        ("folder", "folder_missing"),
    ] {
        let error = pins.pin(&allow, "creator", kind, id).await.unwrap_err();
        assert_eq!(
            (error.status, error.error.code.as_str()),
            (StatusCode::NOT_FOUND, "pin_item_not_found")
        );
    }
    for (kind, id) in [("machine", "machine-a"), ("agent", "a/b")] {
        let error = pins.pin(&allow, "creator", kind, id).await.unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }
    // A revoked agent's pin disappears from reads.
    sqlx::query("UPDATE agent_credentials SET revoked_at='now' WHERE agent_id='agent-a'")
        .execute(&pins.auth.pool())
        .await
        .unwrap();
    let listed = pins.list(&allow, "creator").await.unwrap();
    assert!(listed.pins.iter().all(|pin| pin.id != "agent-a"));

    // Unpin: 204 + one signal; repeating is 204 with neither audit nor signal.
    while next_pins_event(&mut events).await.is_some() {}
    let audits = pins.audit_count().await;
    assert_eq!(
        pins.unpin(&allow, "viewer", "app", "app_one")
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    let event = next_pins_event(&mut events).await.expect("unpinned signal");
    assert_eq!(event.data["action"], "unpinned");
    assert_eq!(
        pins.unpin(&allow, "viewer", "app", "app_one")
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(pins.audit_count().await, audits + 1);
    assert!(next_pins_event(&mut events).await.is_none());
}

#[tokio::test]
async fn pins_limit_is_per_workspace() {
    let pins = fixture().await;
    let allow = PolicyEngine::allow_all();
    let mut folders = Vec::new();
    for n in 0..WORKSPACE_PINS_LIMIT {
        folders.push(folder(&pins.auth, &format!("folder-{n}")).await);
    }
    pins.pin(&allow, "creator", "agent", "agent-a")
        .await
        .unwrap();
    for folder in &folders[..WORKSPACE_PINS_LIMIT - 1] {
        pins.pin(&allow, "creator", "folder", folder).await.unwrap();
    }
    let error = pins
        .pin(
            &allow,
            "creator",
            "folder",
            &folders[WORKSPACE_PINS_LIMIT - 1],
        )
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::CONFLICT, "pins_limit_reached")
    );
    // A re-pin of an existing item at the limit is still 200.
    assert!(pins.pin(&allow, "viewer", "agent", "agent-a").await.is_ok());
    // A revoked Agent's invisible pin does not hold a slot.
    sqlx::query("UPDATE agent_credentials SET revoked_at='now' WHERE agent_id='agent-a'")
        .execute(&pins.auth.pool())
        .await
        .unwrap();
    pins.pin(
        &allow,
        "creator",
        "folder",
        &folders[WORKSPACE_PINS_LIMIT - 1],
    )
    .await
    .expect("the dangling pin does not count");
    assert_eq!(
        pins.list(&allow, "creator").await.unwrap().pins.len(),
        WORKSPACE_PINS_LIMIT
    );
}

#[tokio::test]
async fn pins_follow_tree_authorization_for_reads_and_writes() {
    let pins = fixture().await;
    let allow = PolicyEngine::allow_all();
    pins.pin(&allow, "creator", "agent", "agent-a")
        .await
        .unwrap();
    // Non-member: refused before anything is read.
    let error = pins.list(&allow, "outsider").await.unwrap_err();
    assert!(matches!(
        error.status,
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ));
    let error = pins
        .pin(&allow, "outsider", "agent", "agent-b")
        .await
        .unwrap_err();
    assert!(matches!(
        error.status,
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ));
    // Tree read denied by Policy: no list at all.
    let denied = PolicyEngine::new(
        crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
            "policy_denied",
            "denied by fixture",
        )),
        vec![],
    );
    let error = pins.list(&denied, "creator").await.unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    // Tree read allowed, manage denied: sees the pins, cannot change them.
    let store = WorkspacePolicyStore::new(pins.auth.pool());
    store
        .replace(
            WS,
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([
                    ("agent.tree.read".into(), PolicyEffect::Allow),
                    ("agent.tree.manage".into(), PolicyEffect::Deny),
                ]),
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
    let read_only = PolicyEngine::durable(store);
    let viewer = pins.list(&read_only, "viewer").await.unwrap();
    assert_eq!(viewer.pins.len(), 1);
    assert!(!viewer.can_pin, "the hint fails closed");
    let mut events = pins.state.subscribe();
    let audits = pins.audit_count().await;
    let error = pins
        .pin(&read_only, "viewer", "agent", "agent-b")
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    let error = pins
        .unpin(&read_only, "viewer", "agent", "agent-a")
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(
        pins.audit_count().await,
        audits,
        "denied writes leave no trace"
    );
    assert!(next_pins_event(&mut events).await.is_none());
}
