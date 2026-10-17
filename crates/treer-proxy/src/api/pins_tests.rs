//! Workspace-shared Overview pins (#140) against isolated PostgreSQL: Tree
//! authorisation for reads and writes, idempotency without duplicate audit or
//! events, the per-workspace limit and its race, visibility filtering and
//! cascades, and the IDs-only live signal.
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
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('creator','creator@example.test','Creator','fixture','now'),('viewer','viewer@example.test','Viewer','fixture','now'),('outsider','outsider@example.test','Outsider','fixture','now'),('ephemeral','ephemeral@example.test','Ephemeral','fixture','now')")
        .execute(&auth.pool()).await.unwrap();
    for user in ["creator", "viewer", "ephemeral"] {
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

async fn list(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    user: &str,
) -> Result<WorkspacePinsResponse, ApiFailure> {
    super::pins::list_workspace_pins(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(session(user)),
        HeaderMap::new(),
        Path(WS.into()),
    )
    .await
    .map(|json| json.0)
}

async fn pin(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    user: &str,
    kind: &str,
    id: &str,
) -> Result<Value, ApiFailure> {
    let future = super::pins::pin_workspace_item(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(session(user)),
        HeaderMap::new(),
        Path((WS.into(), kind.into(), id.into())),
    );
    // Production middleware scopes the mutation actor from the session; the
    // tests do the same so audit rows name the member.
    MUTATION_ACTOR
        .scope(
            MutationActor {
                kind: "user",
                id: Some(user.to_owned()),
            },
            future,
        )
        .await
        .map(|json| json.0)
}

async fn unpin(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    user: &str,
    kind: &str,
    id: &str,
) -> Result<StatusCode, ApiFailure> {
    let future = super::pins::unpin_workspace_item(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(session(user)),
        HeaderMap::new(),
        Path((WS.into(), kind.into(), id.into())),
    );
    MUTATION_ACTOR
        .scope(
            MutationActor {
                kind: "user",
                id: Some(user.to_owned()),
            },
            future,
        )
        .await
}

async fn audit_rows(auth: &AuthStore, action: &str) -> Vec<treer_protocol::OrganizationAuditEvent> {
    crate::audit::list(&auth.pool(), &format!("org_{WS}"), Some(WS), None, 500)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.action == action)
        .collect()
}

async fn next_pins_event(
    events: &mut tokio::sync::broadcast::Receiver<treer_protocol::WorkspaceEvent>,
) -> Option<treer_protocol::WorkspaceEvent> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(event)) if event.event == WORKSPACE_PINS_CHANGED_EVENT => return Some(event),
            Ok(Ok(_)) => continue,
            _ => return None,
        }
    }
}

fn read_only_policy(auth: &AuthStore) -> impl std::future::Future<Output = PolicyEngine> {
    let store = WorkspacePolicyStore::new(auth.pool());
    async move {
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
        PolicyEngine::durable(store)
    }
}

#[tokio::test]
async fn pins_round_trip_is_idempotent_filtered_and_audited_with_ids_only() {
    let (state, auth) = seeded().await;
    let allow = PolicyEngine::allow_all();
    let mut events = state.subscribe();

    let empty = list(&state, &auth, &allow, "creator").await.unwrap();
    assert!(empty.pins.is_empty());
    assert_eq!(empty.limit, WORKSPACE_PINS_LIMIT);
    assert!(empty.can_pin);

    let folder = auth
        .create_workload_folder(
            WS,
            treer_protocol::CreateWorkloadFolderRequest {
                name: "research".into(),
                parent_id: None,
            },
        )
        .await
        .unwrap();
    let first = pin(&state, &auth, &allow, "creator", "agent", "agent-a")
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
    let listed = list(&state, &auth, &allow, "creator").await.unwrap();
    assert_eq!(listed.revision, event.revision, "GET reconciles the signal");

    // Idempotent re-pin: same row, no second audit row, no second event.
    let again = pin(&state, &auth, &allow, "viewer", "agent", "agent-a")
        .await
        .unwrap();
    assert_eq!(again["pinned_at"], first["pinned_at"]);
    assert_eq!(again["pinned_by"], "creator");
    assert_eq!(audit_rows(&auth, "tree.pin.create").await.len(), 1);
    assert!(next_pins_event(&mut events).await.is_none());

    pin(&state, &auth, &allow, "viewer", "app", "app_one")
        .await
        .unwrap();
    pin(
        &state,
        &auth,
        &allow,
        "creator",
        "folder",
        &folder.folder_id,
    )
    .await
    .unwrap();
    let listed = list(&state, &auth, &allow, "creator").await.unwrap();
    assert_eq!(
        listed
            .pins
            .iter()
            .map(|pin| (pin.kind, pin.id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (WorkspacePinKind::Folder, folder.folder_id.as_str()),
            (WorkspacePinKind::App, "app_one"),
            (WorkspacePinKind::Agent, "agent-a"),
        ],
        "newest first"
    );
    let serialized = serde_json::to_string(&listed).unwrap();
    assert!(!serialized.contains("research") && !serialized.contains("reviewer"));

    // Audit: kind + stable ID + actor, nothing else.
    let created = audit_rows(&auth, "tree.pin.create").await;
    assert_eq!(created.len(), 3);
    for row in &created {
        assert_eq!(row.actor_kind, "user");
        assert!(matches!(
            row.actor_id.as_deref(),
            Some("creator" | "viewer")
        ));
        assert_eq!(row.payload, json!({}));
        assert!(matches!(
            row.resource_kind.as_str(),
            "agent" | "app" | "folder"
        ));
    }
    assert!(created
        .iter()
        .any(|row| row.resource_id == folder.folder_id));

    // Unknown item, another workspace's App, revoked agent: uniform 404.
    for (kind, id) in [
        ("agent", "ag_missing"),
        ("app", "app_other"),
        ("folder", "folder_missing"),
    ] {
        let error = pin(&state, &auth, &allow, "creator", kind, id)
            .await
            .unwrap_err();
        assert_eq!(
            (error.status, error.error.code.as_str()),
            (StatusCode::NOT_FOUND, "pin_item_not_found"),
            "{kind}/{id}"
        );
    }
    sqlx::query("UPDATE agent_credentials SET revoked_at='now' WHERE agent_id='agent-b'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert_eq!(
        pin(&state, &auth, &allow, "creator", "agent", "agent-b")
            .await
            .unwrap_err()
            .error
            .code,
        "pin_item_not_found"
    );
    for (kind, id, code) in [
        ("machine", "machine-a", "pin_kind_invalid"),
        ("agent", "", "pin_id_invalid"),
        ("agent", "a/b", "pin_id_invalid"),
        ("agent", &"x".repeat(129), "pin_id_invalid"),
    ] {
        let error = pin(&state, &auth, &allow, "creator", kind, id)
            .await
            .unwrap_err();
        assert_eq!(
            (error.status, error.error.code.as_str()),
            (StatusCode::BAD_REQUEST, code)
        );
    }

    // Visibility: a revoked (deleted) agent's pin disappears from reads; an
    // App or folder deletion cascades; a deleted pinner leaves the pin.
    while next_pins_event(&mut events).await.is_some() {}
    sqlx::query("UPDATE agent_credentials SET revoked_at='now' WHERE agent_id='agent-a'")
        .execute(&auth.pool())
        .await
        .unwrap();
    let listed = list(&state, &auth, &allow, "creator").await.unwrap();
    assert!(listed.pins.iter().all(|pin| pin.id != "agent-a"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM workspace_pins WHERE agent_id='agent-a'"
        )
        .fetch_one(&auth.pool())
        .await
        .unwrap(),
        1,
        "GET filters, it does not write"
    );
    auth.delete_workload_folder(WS, &folder.folder_id)
        .await
        .unwrap();
    sqlx::query("DELETE FROM app_deployments WHERE app_id='app_one'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workspace_pins WHERE workspace_id=$1")
            .bind(WS)
            .fetch_one(&auth.pool())
            .await
            .unwrap(),
        1,
        "app and folder pins cascaded; the revoked agent row remains until its deletion path"
    );
    sqlx::query("UPDATE agent_credentials SET revoked_at=NULL WHERE agent_id='agent-a'")
        .execute(&auth.pool())
        .await
        .unwrap();
    // Pinned by a member who is later deleted.
    unpin(&state, &auth, &allow, "creator", "agent", "agent-a")
        .await
        .unwrap();
    pin(&state, &auth, &allow, "ephemeral", "agent", "agent-a")
        .await
        .unwrap();
    sqlx::query("DELETE FROM organization_members WHERE user_id='ephemeral'")
        .execute(&auth.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id='ephemeral'")
        .execute(&auth.pool())
        .await
        .unwrap();
    let listed = list(&state, &auth, &allow, "creator").await.unwrap();
    assert_eq!(listed.pins.len(), 1);
    assert_eq!(
        listed.pins[0].pinned_by, None,
        "shared pin outlives its pinner"
    );

    // Unpin: 204 + one audit + one event; repeating is 204 with neither.
    while next_pins_event(&mut events).await.is_some() {}
    let deletes_before = audit_rows(&auth, "tree.pin.delete").await.len();
    assert_eq!(
        unpin(&state, &auth, &allow, "viewer", "agent", "agent-a")
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    let event = next_pins_event(&mut events).await.expect("unpinned signal");
    assert_eq!(event.data["action"], "unpinned");
    assert_eq!(event.data["id"], "agent-a");
    assert_eq!(
        audit_rows(&auth, "tree.pin.delete").await.len(),
        deletes_before + 1
    );
    assert_eq!(
        unpin(&state, &auth, &allow, "viewer", "agent", "agent-a")
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        audit_rows(&auth, "tree.pin.delete").await.len(),
        deletes_before + 1
    );
    assert!(next_pins_event(&mut events).await.is_none());
    assert!(list(&state, &auth, &allow, "creator")
        .await
        .unwrap()
        .pins
        .is_empty());
    let all_audit = sqlx::query_scalar::<_, String>(
        "SELECT payload::text FROM organization_audit_events WHERE action LIKE 'tree.pin.%'",
    )
    .fetch_all(&auth.pool())
    .await
    .unwrap()
    .join("\n");
    assert!(!all_audit.contains("research") && !all_audit.contains("reviewer"));
}

#[tokio::test]
async fn pins_limit_is_per_workspace_and_the_last_slot_race_admits_one() {
    let (state, auth) = seeded().await;
    let allow = PolicyEngine::allow_all();
    let mut folders = Vec::new();
    for n in 0..WORKSPACE_PINS_LIMIT {
        folders.push(
            auth.create_workload_folder(
                WS,
                treer_protocol::CreateWorkloadFolderRequest {
                    name: format!("folder-{n}"),
                    parent_id: None,
                },
            )
            .await
            .unwrap()
            .folder_id,
        );
    }
    pin(&state, &auth, &allow, "creator", "agent", "agent-a")
        .await
        .unwrap();
    for folder in &folders[..WORKSPACE_PINS_LIMIT - 1] {
        pin(&state, &auth, &allow, "creator", "folder", folder)
            .await
            .unwrap();
    }
    let error = pin(
        &state,
        &auth,
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
    assert!(pin(&state, &auth, &allow, "viewer", "agent", "agent-a")
        .await
        .is_ok());
    assert_eq!(
        list(&state, &auth, &allow, "creator")
            .await
            .unwrap()
            .pins
            .len(),
        WORKSPACE_PINS_LIMIT
    );
    assert_eq!(
        audit_rows(&auth, "tree.pin.create").await.len(),
        WORKSPACE_PINS_LIMIT
    );

    // Free one slot, then race two new items for it: exactly one wins.
    unpin(&state, &auth, &allow, "creator", "folder", &folders[0])
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        pin(&state, &auth, &allow, "creator", "folder", &folders[0]),
        pin(
            &state,
            &auth,
            &allow,
            "viewer",
            "folder",
            &folders[WORKSPACE_PINS_LIMIT - 1]
        ),
    );
    let outcomes = [a.is_ok(), b.is_ok()];
    assert_eq!(outcomes.iter().filter(|ok| **ok).count(), 1, "{outcomes:?}");
    let loser = [a, b].into_iter().find_map(Result::err).unwrap();
    assert_eq!(loser.error.code, "pins_limit_reached");
    assert_eq!(
        list(&state, &auth, &allow, "creator")
            .await
            .unwrap()
            .pins
            .len(),
        WORKSPACE_PINS_LIMIT
    );
}

#[tokio::test]
async fn pins_follow_tree_authorization_for_reads_and_writes() {
    let (state, auth) = seeded().await;
    let allow = PolicyEngine::allow_all();
    pin(&state, &auth, &allow, "creator", "agent", "agent-a")
        .await
        .unwrap();

    // Non-member: refused before anything is read.
    let error = list(&state, &auth, &allow, "outsider").await.unwrap_err();
    assert!(matches!(
        error.status,
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ));
    let error = pin(&state, &auth, &allow, "outsider", "agent", "agent-b")
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
    assert_eq!(
        list(&state, &auth, &denied, "creator")
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );

    // Tree read allowed, manage denied: sees the pins, cannot change them.
    let read_only = read_only_policy(&auth).await;
    let viewer = list(&state, &auth, &read_only, "viewer").await.unwrap();
    assert_eq!(viewer.pins.len(), 1);
    assert!(!viewer.can_pin, "the hint fails closed");
    let mut events = state.subscribe();
    let audits_before = audit_rows(&auth, "tree.pin.create").await.len()
        + audit_rows(&auth, "tree.pin.delete").await.len();
    let error = pin(&state, &auth, &read_only, "viewer", "agent", "agent-b")
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    let error = unpin(&state, &auth, &read_only, "viewer", "agent", "agent-a")
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(
        audit_rows(&auth, "tree.pin.create").await.len()
            + audit_rows(&auth, "tree.pin.delete").await.len(),
        audits_before,
        "a denied write leaves no trace in the pin audit"
    );
    assert!(next_pins_event(&mut events).await.is_none());
    assert_eq!(
        list(&state, &auth, &allow, "creator")
            .await
            .unwrap()
            .pins
            .len(),
        1
    );

    // can_pin is only a hint: a PUT after permissions change authorises itself.
    assert!(list(&state, &auth, &allow, "viewer").await.unwrap().can_pin);
    assert_eq!(
        pin(&state, &auth, &read_only, "viewer", "agent", "agent-b")
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
}
