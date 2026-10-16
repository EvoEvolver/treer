use super::*;
use crate::auth::AuthStore;

async fn fixture() -> AuthStore {
    let auth = AuthStore::for_test("synthetic").await;
    auth.seed_test_workspace("audit").await;
    auth
}

fn actor() -> MutationActor {
    MutationActor {
        kind: "agent",
        id: Some("ag_sender".into()),
    }
}

#[tokio::test]
async fn durable_admission_is_bounded_across_connections_and_releases_capacity() {
    let auth = fixture().await;
    let pool = auth.pool();
    sqlx::query("UPDATE audit_maintenance_settings SET queue_capacity=3")
        .execute(&pool)
        .await
        .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let pool = pool.clone();
        tasks.spawn(async move {
            enqueue_prompt(&pool, "audit", "ag_target", actor())
                .await
                .unwrap()
        });
    }
    let mut admitted = 0;
    while let Some(result) = tasks.join_next().await {
        admitted += usize::from(result.unwrap());
    }
    assert_eq!(admitted, 3);
    let counters: (i32, i64) =
        sqlx::query_as("SELECT pending,overflow_total FROM audit_maintenance_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(counters, (3, 17));
    assert_eq!(drain_batch(&pool).await.unwrap(), 3);
    assert_eq!(drain_batch(&pool).await.unwrap(), 0);
    let pending: i32 = sqlx::query_scalar("SELECT pending FROM audit_maintenance_settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 0);
    assert!(enqueue_prompt(&pool, "audit", "ag_target", actor())
        .await
        .unwrap());
    sqlx::query("DELETE FROM organizations WHERE organization_id='org_audit'")
        .execute(&pool)
        .await
        .unwrap();
    let pending: i32 = sqlx::query_scalar("SELECT pending FROM audit_maintenance_settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 0, "cascade deletion releases capacity too");
}

#[tokio::test]
async fn worker_replays_once_and_captures_actor_without_task_local_inheritance() {
    let auth = fixture().await;
    let pool = auth.pool();
    MUTATION_ACTOR
        .scope(actor(), async {
            let captured = capture_actor();
            enqueue_prompt(&pool, "audit", "ag_target", captured)
                .await
                .unwrap();
        })
        .await;
    // Simulate an uncertain earlier commit: drain must not duplicate this ID.
    sqlx::query("INSERT INTO organization_audit_events(event_id,organization_id,workspace_id,occurred_at,actor_kind,actor_id,source,action,outcome,resource_kind,resource_id,payload,workspace_origin_id) \
        SELECT event_id,organization_id,workspace_id,occurred_at,actor_kind,actor_id,'control-plane','agent.prompt','succeeded','agent',resource_id,'{}',workspace_origin_id FROM prompt_audit_outbox")
        .execute(&pool).await.unwrap();
    let (a, b) = tokio::join!(drain_batch(&pool), drain_batch(&pool));
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    let rows = list(&pool, "org_audit", None, None, 100).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor_id.as_deref(), Some("ag_sender"));
    assert_eq!(rows[0].payload, serde_json::json!({}));
    enqueue_prompt(&pool, "audit", "ag_target", actor())
        .await
        .unwrap();
    Worker::start(pool.clone()).shutdown().await;
    assert_eq!(
        list(&pool, "org_audit", None, None, 100)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn retention_is_batched_uniform_and_metadata_is_tenant_scoped() {
    let auth = fixture().await;
    auth.seed_test_workspace("other").await;
    let pool = auth.pool();
    // Mixed sensitive and high-volume kinds get exactly the same deadline.
    sqlx::query("INSERT INTO organization_audit_events(event_id,organization_id,workspace_id,workspace_origin_id,occurred_at,actor_kind,source,action,outcome,resource_kind,resource_id) \
        SELECT 'old_'||i,'org_audit','audit','audit',(CURRENT_TIMESTAMP-INTERVAL '181 days')::TEXT,'system','test', \
        CASE i%3 WHEN 0 THEN 'auth.login' WHEN 1 THEN 'policy.publish' ELSE 'agent.prompt' END,'succeeded','workspace','audit' FROM generate_series(1,300) AS i")
        .execute(&pool).await.unwrap();
    enqueue_prompt(&pool, "other", "ag_other", actor())
        .await
        .unwrap();
    enqueue_prompt(&pool, "audit", "ag_target", actor())
        .await
        .unwrap();
    drain_batch(&pool).await.unwrap();
    assert_eq!(cleanup_batch(&pool).await.unwrap(), 256);
    assert_eq!(cleanup_batch(&pool).await.unwrap(), 44);
    assert_eq!(cleanup_batch(&pool).await.unwrap(), 0);
    let meta = metadata(&pool, None, Some("audit"), true).await.unwrap();
    assert_eq!(meta.retention_days, 180);
    assert!(meta.deleted_through.is_some());
    let stats = meta.rows_per_day.unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].rows, 1);
    let other = metadata(&pool, None, Some("other"), true).await.unwrap();
    assert!(other.deleted_through.is_none());
    let org = metadata(&pool, Some("org_audit"), None, false)
        .await
        .unwrap();
    assert_eq!(org.deleted_through, meta.deleted_through);
    configure(&pool, Some(365)).await.unwrap();
    configure(&pool, None).await.unwrap();
    let increased = metadata(&pool, None, Some("audit"), false).await.unwrap();
    assert_eq!(increased.retention_days, 365);
    assert_eq!(
        increased.deleted_through, meta.deleted_through,
        "increasing retention cannot restore history"
    );
}

#[tokio::test]
async fn one_cleanup_cycle_catches_up_multiple_batches_and_configuration_is_audited() {
    let auth = fixture().await;
    let pool = auth.pool();
    sqlx::query("INSERT INTO organization_audit_events(event_id,organization_id,workspace_id,occurred_at,actor_kind,source,action,outcome,resource_kind,resource_id) \
        SELECT 'backlog_'||i,'org_audit','audit',(CURRENT_TIMESTAMP-INTERVAL '181 days')::TEXT,'system','test','policy.publish','succeeded','workspace','audit' FROM generate_series(1,700) AS i")
        .execute(&pool).await.unwrap();
    assert!(metadata(&pool, None, Some("audit"), true)
        .await
        .unwrap()
        .oldest_expired_at
        .is_some());
    // Test catch-up semantics, not local database speed under the full parallel
    // suite. Production always supplies a one-second budget; batch cap stays 64.
    assert!(!cleanup_with_budget(&pool, Duration::from_secs(60))
        .await
        .unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM organization_audit_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert!(metadata(&pool, None, Some("audit"), true)
        .await
        .unwrap()
        .oldest_expired_at
        .is_none());
    assert!(configure(&pool, Some(1)).await.is_err());
    assert_eq!(
        metadata(&pool, None, Some("audit"), false)
            .await
            .unwrap()
            .retention_days,
        180
    );
    configure(&pool, Some(30)).await.unwrap();
    configure(&pool, None).await.unwrap();
    let payloads: Vec<Value> = sqlx::query_scalar(
        "SELECT payload FROM platform_audit_events WHERE action='audit.retention.configure'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        payloads,
        vec![serde_json::json!({"previous_days":180,"retention_days":30})]
    );
}

#[tokio::test]
async fn failed_insert_does_not_consume_capacity_and_failed_drain_keeps_durable_entry() {
    let auth = fixture().await;
    let pool = auth.pool();
    assert!(enqueue_prompt(&pool, "missing", "target", actor())
        .await
        .is_err());
    let pending: i32 = sqlx::query_scalar("SELECT pending FROM audit_maintenance_settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 0);
    enqueue_prompt(&pool, "audit", "target", actor())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE organization_audit_events ADD CONSTRAINT reject_prompt_test CHECK(action<>'agent.prompt') NOT VALID").execute(&pool).await.unwrap();
    assert!(drain_batch(&pool).await.is_err());
    let pending: i32 = sqlx::query_scalar("SELECT pending FROM audit_maintenance_settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 1);
    sqlx::query("ALTER TABLE organization_audit_events DROP CONSTRAINT reject_prompt_test")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(drain_batch(&pool).await.unwrap(), 1);
}

#[tokio::test]
async fn prompt_audit_latency_measurement() {
    let auth = fixture().await;
    let pool = auth.pool();
    let mut direct = Vec::new();
    let mut queued = Vec::new();
    for _ in 0..100 {
        let started = std::time::Instant::now();
        mutation_best_effort(&pool, "audit", "agent.prompt", "agent", "target").await;
        direct.push(started.elapsed().as_micros());
        let started = std::time::Instant::now();
        assert!(enqueue_prompt(&pool, "audit", "target", actor())
            .await
            .unwrap());
        queued.push(started.elapsed().as_micros());
    }
    direct.sort_unstable();
    queued.sort_unstable();
    eprintln!("audit latency local_pg samples=100 direct_p50_us={} direct_p95_us={} enqueue_p50_us={} enqueue_p95_us={}",direct[50],direct[95],queued[50],queued[95]);
    // Measurements are evidence, not a flaky timing assertion or speed promise.
}
