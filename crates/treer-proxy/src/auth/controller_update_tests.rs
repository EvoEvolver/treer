//! Isolated PostgreSQL tests; no machine process or updater is invoked.
use super::controller_update::*;
use super::*;
use chrono::Utc;
use treer_protocol::{controller_update as wire, controller_update_runtime::DispatchBinding};

async fn store() -> AuthStore {
    let auth = AuthStore::for_test("fixture").await;
    auth.seed_test_workspace("default").await;
    auth
}
fn scope() -> ControllerUpdateScope<'static> {
    ControllerUpdateScope {
        workspace_id: "default",
        server_id: "machine-a",
        user_id: "creator",
        session_token: "private-test-session",
    }
}
fn identity(c: char) -> wire::Identity {
    wire::Identity {
        release_sha: c.to_string().repeat(40),
        artifact_source_sha: c.to_string().repeat(40),
        controller_sha256: c.to_string().repeat(64),
        cli_sha256: c.to_string().repeat(64),
    }
}
fn binding() -> DispatchBinding {
    DispatchBinding {
        operation_id: Uuid::new_v4().to_string(),
        workspace_id: "default".into(),
        server_id: "machine-a".into(),
        current: identity('a'),
        target: wire::Target {
            target_id: "release-b".into(),
            identity: identity('b'),
            platform: wire::Platform::LinuxX86_64,
            version_label: "1.2.0".into(),
        },
        catalog_revision: 2,
        plan_digest: "c".repeat(64),
        created_at: Utc::now(),
    }
}
async fn preview(auth: &AuthStore, binding: &DispatchBinding) -> wire::Preview {
    auth.controller_update_prepare_begin(&scope())
        .await
        .unwrap();
    auth.controller_update_preview(&scope(), binding)
        .await
        .unwrap()
}
fn succeeded(mut op: wire::Operation) -> wire::Operation {
    op.state = wire::State::Succeeded;
    op.error_code = None;
    op.phase = Some(wire::Phase::Complete);
    op.observation = wire::Observation::Available;
    op.health = wire::Health::Healthy;
    op.receipt_revision = Some(10);
    op.updated_at = Utc::now().max(op.updated_at);
    op.observed_at = Some(op.updated_at);
    op.finished_at = Some(op.updated_at);
    op.observed_identity = Some(op.target.identity.clone());
    op
}

#[tokio::test]
async fn concurrent_confirmation_grants_only_one_dispatch_and_survives_reload() {
    let auth = store().await;
    let b = binding();
    let p = preview(&auth, &b).await;
    let s = scope();
    let (a, c) = tokio::join!(
        auth.controller_update_accept(&s, &p.confirmation_token, &b),
        auth.controller_update_accept(&s, &p.confirmation_token, &b)
    );
    let a = a.unwrap();
    let c = c.unwrap();
    assert_ne!(a.dispatch.is_some(), c.dispatch.is_some());
    assert_eq!(a.operation, c.operation);
    // New store object with no update cache reads the committed intent.
    let reloaded = auth.clone();
    assert_eq!(
        reloaded
            .controller_update_operation(&s, &b.operation_id)
            .await
            .unwrap(),
        a.operation
    );
    assert!(!reloaded
        .controller_update_query_lease(&s, "missing")
        .await
        .unwrap());
    assert!(reloaded
        .controller_update_query_lease(&s, &b.operation_id)
        .await
        .unwrap());
    assert!(!auth
        .controller_update_query_lease(&s, &b.operation_id)
        .await
        .unwrap());
    let stored: String = sqlx::query_scalar(
        "SELECT binding::text || operation::text FROM controller_update_operations",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert!(!stored.contains(&p.confirmation_token));
    assert!(!stored.contains(s.session_token));
}

#[tokio::test]
async fn foreign_session_token_and_creator_lookup_are_uniformly_refused() {
    let auth = store().await;
    let b = binding();
    let p = preview(&auth, &b).await;
    let mut foreign = scope();
    foreign.session_token = "other-session";
    assert!(auth
        .controller_update_confirmation(&foreign, &p.confirmation_token)
        .await
        .is_err());
    assert!(auth
        .controller_update_accept(&foreign, &p.confirmation_token, &b)
        .await
        .is_err());
    auth.controller_update_accept(&scope(), &p.confirmation_token, &b)
        .await
        .unwrap();
    foreign.user_id = "other";
    let e = auth
        .controller_update_operation(&foreign, &b.operation_id)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "controller_update_operation_not_found");
    let d = auth.controller_update_discovery(&foreign).await.unwrap();
    assert!(d.operations.is_empty());
    assert!(d.blocked_by_other_operation);
}

#[tokio::test]
async fn unresolved_claim_is_not_evicted_by_time_or_another_prepare() {
    let auth = store().await;
    let b = binding();
    let p = preview(&auth, &b).await;
    let accepted = auth
        .controller_update_accept(&scope(), &p.confirmation_token, &b)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE controller_update_operations SET created_at=CURRENT_TIMESTAMP-INTERVAL '10 days'",
    )
    .execute(&auth.pool())
    .await
    .unwrap();
    assert!(auth
        .controller_update_preview(&scope(), &binding())
        .await
        .is_err());
    assert_eq!(
        auth.controller_update_discovery(&scope())
            .await
            .unwrap()
            .operations,
        vec![accepted.operation]
    );
}

#[tokio::test]
async fn succeeded_operation_allows_a_new_explicit_fresh_preview() {
    let auth = store().await;
    let b = binding();
    let p = preview(&auth, &b).await;
    let a = auth
        .controller_update_accept(&scope(), &p.confirmation_token, &b)
        .await
        .unwrap();
    let done = succeeded(a.operation);
    auth.controller_update_observe(&scope(), &done)
        .await
        .unwrap();
    let mut next = binding();
    next.current = b.target.identity;
    next.target.identity = identity('d');
    let p = preview(&auth, &next).await;
    assert_ne!(p.operation_id, done.operation_id);
    assert!(auth
        .controller_update_accept(&scope(), &p.confirmation_token, &next)
        .await
        .unwrap()
        .dispatch
        .is_some());
}

#[tokio::test]
async fn conflicting_equal_or_regressed_receipts_and_future_reports_are_refused() {
    let auth = store().await;
    let b = binding();
    let p = preview(&auth, &b).await;
    let a = auth
        .controller_update_accept(&scope(), &p.confirmation_token, &b)
        .await
        .unwrap();
    let mut running = a.operation;
    running.state = wire::State::Updating;
    running.phase = Some(wire::Phase::Prepared);
    running.receipt_revision = Some(2);
    running.updated_at = Utc::now().max(running.updated_at);
    auth.controller_update_observe(&scope(), &running)
        .await
        .unwrap();
    let mut bad = running.clone();
    bad.phase = Some(wire::Phase::Activating);
    assert!(auth
        .controller_update_observe(&scope(), &bad)
        .await
        .is_err());
    bad.receipt_revision = Some(1);
    assert!(auth
        .controller_update_observe(&scope(), &bad)
        .await
        .is_err());
    bad.receipt_revision = Some(3);
    bad.updated_at = Utc::now() + chrono::Duration::seconds(31);
    assert!(auth
        .controller_update_observe(&scope(), &bad)
        .await
        .is_err());
    assert_eq!(
        auth.controller_update_operation(&scope(), &b.operation_id)
            .await
            .unwrap(),
        running
    );
}

#[tokio::test]
async fn partial_receipt_holds_slot_and_cannot_be_overwritten_by_fake_success() {
    let auth = store().await;
    let b = binding();
    let p = preview(&auth, &b).await;
    let a = auth
        .controller_update_accept(&scope(), &p.confirmation_token, &b)
        .await
        .unwrap();
    let mut partial = a.operation;
    partial.state = wire::State::Partial;
    partial.phase = Some(wire::Phase::Partial);
    partial.receipt_revision = Some(4);
    partial.updated_at = Utc::now().max(partial.updated_at);
    partial.finished_at = Some(partial.updated_at);
    partial.error_code = Some(wire::UpdateError::RecoveryRequired);
    auth.controller_update_observe(&scope(), &partial)
        .await
        .unwrap();
    assert!(auth
        .controller_update_preview(&scope(), &binding())
        .await
        .is_err());
    assert!(auth
        .controller_update_observe(&scope(), &succeeded(partial))
        .await
        .is_err());
}

#[tokio::test]
async fn catalog_has_explicit_upgrade_order_and_persistent_antirollback_floor() {
    let auth = store().await;
    let b = binding();
    let mut current = b.target.clone();
    current.identity = b.current.clone();
    current.target_id = "release-a".into();
    for (sequence, target) in [(1, current), (2, b.target.clone())] {
        sqlx::query("INSERT INTO controller_update_catalog_entries(platform,revision,release_sequence,target) VALUES('linux-x86_64',2,$1,$2)").bind(sequence as i64).bind(serde_json::to_value(target).unwrap()).execute(&auth.pool()).await.unwrap();
    }
    let (revision, targets) = auth
        .controller_update_targets(b.target.platform, &b.current)
        .await
        .unwrap();
    assert_eq!(revision, 2);
    assert_eq!(targets, vec![b.target.clone()]);
    assert!(auth
        .controller_update_targets(b.target.platform, &identity('f'))
        .await
        .is_err());
    sqlx::query("UPDATE controller_update_catalog_entries SET revision=1")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert!(auth
        .controller_update_targets(b.target.platform, &b.current)
        .await
        .is_err());
}

#[tokio::test]
async fn update_claim_also_refuses_installer_acceptance_before_consumption() {
    let auth = store().await;
    let b = binding();
    let p = preview(&auth, &b).await;
    auth.controller_update_accept(&scope(), &p.confirmation_token, &b)
        .await
        .unwrap();
    let error = auth
        .provider_cli_accept(&scope(), &format!("pcl_{}", "0".repeat(64)), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "provider_cli_machine_busy");
}

#[tokio::test]
async fn silence_becomes_unconfirmed_without_releasing_or_resending() {
    let auth = store().await;
    let mut b = binding();
    b.created_at -= chrono::Duration::seconds(700);
    let p = preview(&auth, &b).await;
    let mut a = auth
        .controller_update_accept(&scope(), &p.confirmation_token, &b)
        .await
        .unwrap()
        .operation;
    a.updated_at = b.created_at;
    sqlx::query("UPDATE controller_update_operations SET operation=$2 WHERE operation_id=$1")
        .bind(&b.operation_id)
        .bind(serde_json::to_value(&a).unwrap())
        .execute(&auth.pool())
        .await
        .unwrap();
    let op = auth
        .controller_update_note_silence(&scope(), &b.operation_id)
        .await
        .unwrap();
    assert_eq!(op.state, wire::State::Unconfirmed);
    assert!(auth
        .controller_update_preview(&scope(), &binding())
        .await
        .is_err());
    let again = auth
        .controller_update_accept(&scope(), &p.confirmation_token, &b)
        .await
        .unwrap();
    assert!(again.dispatch.is_none());
    assert_eq!(again.operation, op);
}
