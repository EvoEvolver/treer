
use super::*;
use crate::auth::AuthStore;

#[tokio::test]
async fn agent_usage_persists_separately_without_doubling_machine_usage() {
    let store = AuthStore::for_test("admin-password").await;
    store.seed_test_workspace("agent-traffic").await;
    let recorder = TrafficRecorder::new(store.pool());
    let detail = recorder.agent_view();
    let agent = detail
        .register_agent_stream(
            "agent-traffic",
            "source",
            "destination",
            Some("agent-a"),
            Some("agent-b"),
        )
        .unwrap();
    agent.source_to_destination.record(12);
    agent.destination_to_source.record(18);
    detail.flush_pending().await.unwrap();
    let records = detail.recent("agent-traffic", 1).await.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records.iter().map(|r| r.payload_bytes).sum::<u64>(), 30);
    assert!(records
        .iter()
        .all(|r| r.source_type == "agent" && r.destination_type == "agent"));
    assert!(recorder
        .recent("agent-traffic", 1)
        .await
        .unwrap()
        .is_empty());
    let mut direct = recorder
        .register_direct_stream("agent-traffic", "source", "example.test", 443)
        .with_agent_meter(Some(detail.register_direct_stream(
            "agent-traffic",
            "agent-a",
            "example.test",
            443,
        )));
    direct
        .record(NetworkUsageTotals {
            sent_bytes: 5,
            received_bytes: 7,
            sent_chunks: 1,
            received_chunks: 1,
        })
        .unwrap();
    recorder.flush_pending().await.unwrap();
    detail.flush_pending().await.unwrap();
    assert_eq!(
        recorder
            .recent("agent-traffic", 1)
            .await
            .unwrap()
            .iter()
            .map(|r| r.payload_bytes)
            .sum::<u64>(),
        12
    );
    let records = detail.recent("agent-traffic", 1).await.unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|r| r.traffic_class == "direct_network")
            .map(|r| r.payload_bytes)
            .sum::<u64>(),
        12
    );
}

#[tokio::test]
async fn direct_usage_is_cumulative_deduplicated_and_not_billable() {
    let recorder = TrafficRecorder::default();
    let mut stream = recorder.register_direct_stream("workspace", "source", "example.test", 443);
    let first = NetworkUsageTotals {
        sent_bytes: 12,
        received_bytes: 18,
        sent_chunks: 1,
        received_chunks: 2,
    };
    stream.record(first).unwrap();
    stream.record(first).unwrap();
    assert!(stream
        .record(NetworkUsageTotals {
            sent_bytes: 11,
            ..first
        })
        .is_err());
    stream
        .record(NetworkUsageTotals {
            sent_bytes: 20,
            sent_chunks: 3,
            ..first
        })
        .unwrap();
    let records = recorder.recent("workspace", 1).await.unwrap();
    assert_eq!(records.len(), 2);
    let sent = records.iter().find(|r| r.source_type == "machine").unwrap();
    let received = records
        .iter()
        .find(|r| r.source_type == "internet")
        .unwrap();
    assert_eq!((sent.payload_bytes, sent.payload_frames), (20, 3));
    assert_eq!((received.payload_bytes, received.payload_frames), (18, 2));
    for record in records {
        assert_eq!(record.traffic_class, "direct_network");
        assert_eq!(record.meter_version, 2);
        assert_eq!(record.billable_bytes, 0);
    }
}

#[tokio::test]
async fn durable_direct_receipts_deduplicate_across_proxy_restart_and_bind_identity() {
    let store = AuthStore::for_test("admin-password").await;
    store.seed_test_workspace("durable-usage").await;
    let pool = store.pool();
    let first_proxy = TrafficRecorder::new(pool.clone());
    let ticket = first_proxy
        .issue_usage_ticket(
            "durable-usage",
            "machine-a",
            Some("agent-a"),
            "example.test",
            443,
        )
        .await
        .unwrap()
        .unwrap();
    let first = NetworkUsageReport {
        finished: false,
        ticket,
        totals: NetworkUsageTotals {
            sent_bytes: 12,
            received_bytes: 18,
            sent_chunks: 1,
            received_chunks: 1,
        },
    };
    first_proxy
        .persist_usage_report("durable-usage", "machine-a", &first)
        .await
        .unwrap();
    drop(first_proxy);
    let restarted = TrafficRecorder::new(pool.clone());
    restarted
        .persist_usage_report("durable-usage", "machine-a", &first)
        .await
        .unwrap();
    let mut latest = first.clone();
    latest.totals.sent_bytes = 20;
    latest.totals.sent_chunks = 2;
    restarted
        .persist_usage_report("durable-usage", "machine-a", &latest)
        .await
        .unwrap();
    // Lost acknowledgements and out-of-order replay add no extra bytes.
    restarted
        .persist_usage_report("durable-usage", "machine-a", &first)
        .await
        .unwrap();
    assert!(restarted
        .persist_usage_report("durable-usage", "machine-b", &latest)
        .await
        .is_err());
    assert!(restarted
        .persist_usage_report("other-workspace", "machine-a", &latest)
        .await
        .is_err());
    let mut invalid = latest.clone();
    invalid.totals.sent_bytes = 21;
    invalid.totals.received_bytes = 1;
    assert!(restarted
        .persist_usage_report("durable-usage", "machine-a", &invalid)
        .await
        .is_err());
    for recorder in [restarted.clone(), restarted.agent_view()] {
        let rows = recorder.recent("durable-usage", 1).await.unwrap();
        assert_eq!(rows.iter().map(|row| row.payload_bytes).sum::<u64>(), 38);
        assert!(rows.iter().all(|row| row.billable_bytes == 0));
    }
    sqlx::query("ALTER TABLE agent_traffic_usage_hourly ADD CONSTRAINT test_usage_failure CHECK(payload_bytes < 100)")
            .execute(&pool).await.unwrap();
    let mut retry = latest.clone();
    retry.totals.sent_bytes = 120;
    retry.finished = true;
    assert!(restarted
        .persist_usage_report("durable-usage", "machine-a", &retry)
        .await
        .is_err());
    // The machine upsert happens before Agent detail; failure of the latter
    // must roll back both the former and receipt deduplication state.
    assert_eq!(
        restarted
            .recent("durable-usage", 1)
            .await
            .unwrap()
            .iter()
            .map(|row| row.payload_bytes)
            .sum::<u64>(),
        38
    );
    let closed: Option<i64> =
        sqlx::query_scalar("SELECT closed_at FROM network_usage_receipts WHERE ticket=$1")
            .bind(&retry.ticket)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        closed.is_none(),
        "failed ledger transaction cannot close its receipt"
    );
    sqlx::query("ALTER TABLE agent_traffic_usage_hourly DROP CONSTRAINT test_usage_failure")
        .execute(&pool)
        .await
        .unwrap();
    restarted
        .persist_usage_report("durable-usage", "machine-a", &retry)
        .await
        .unwrap();
    let closed: Option<i64> =
        sqlx::query_scalar("SELECT closed_at FROM network_usage_receipts WHERE ticket=$1")
            .bind(&retry.ticket)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(closed.is_some());
    assert_eq!(
        restarted
            .recent("durable-usage", 1)
            .await
            .unwrap()
            .iter()
            .map(|row| row.payload_bytes)
            .sum::<u64>(),
        138
    );
}

#[tokio::test]
async fn unused_and_expired_open_usage_receipts_are_reclaimed() {
    let store = AuthStore::for_test("admin-password").await;
    store.seed_test_workspace("receipt-cleanup").await;
    let pool = store.pool();
    let recorder = TrafficRecorder::new(pool.clone());
    let unused = recorder
        .issue_usage_ticket(
            "receipt-cleanup",
            "machine-a",
            Some("agent-a"),
            "unused.test",
            443,
        )
        .await
        .unwrap()
        .unwrap();
    recorder
        .abandon_usage_ticket("receipt-cleanup", "machine-b", &unused)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM network_usage_receipts WHERE ticket=$1")
            .bind(&unused)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1,
        "another machine cannot abandon the ticket"
    );
    recorder
        .abandon_usage_ticket("receipt-cleanup", "machine-a", &unused)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM network_usage_receipts WHERE ticket=$1")
            .bind(&unused)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    let expired = recorder
        .issue_usage_ticket(
            "receipt-cleanup",
            "machine-a",
            Some("agent-a"),
            "expired.test",
            443,
        )
        .await
        .unwrap()
        .unwrap();
    let fresh = recorder
        .issue_usage_ticket(
            "receipt-cleanup",
            "machine-a",
            Some("agent-a"),
            "fresh.test",
            443,
        )
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE network_usage_receipts SET created_at=0 WHERE ticket=$1")
        .bind(&expired)
        .execute(&pool)
        .await
        .unwrap();
    recorder.delete_expired().await.unwrap();
    let remaining = sqlx::query_scalar::<_, String>(
        "SELECT ticket FROM network_usage_receipts WHERE workspace_id=$1",
    )
    .bind("receipt-cleanup")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, vec![fresh]);
}

#[tokio::test]
async fn direct_usage_persists_deltas_and_restores_failed_flush() {
    let store = AuthStore::for_test("admin-password").await;
    store.seed_test_workspace("direct-traffic").await;
    let pool = store.pool();
    let recorder = TrafficRecorder::new(pool.clone());
    let mut stream =
        recorder.register_direct_stream("direct-traffic", "source", "example.test", 443);
    let first = NetworkUsageTotals {
        sent_bytes: 12,
        received_bytes: 18,
        sent_chunks: 1,
        received_chunks: 1,
    };
    stream.record(first).unwrap();
    recorder.flush_pending().await.unwrap();
    stream.record(first).unwrap();
    stream
        .record(NetworkUsageTotals {
            sent_bytes: 20,
            sent_chunks: 2,
            ..first
        })
        .unwrap();
    recorder.flush_pending().await.unwrap();
    let records = recorder.recent("direct-traffic", 1).await.unwrap();
    assert_eq!(records.iter().map(|r| r.payload_bytes).sum::<u64>(), 38);
    assert!(records
        .iter()
        .all(|r| r.billable_bytes == 0 && r.meter_version == 2));
    stream
        .record(NetworkUsageTotals {
            sent_bytes: 25,
            sent_chunks: 3,
            ..first
        })
        .unwrap();
    pool.close().await;
    assert!(recorder.flush_pending().await.is_err());
    assert_eq!(
        stream
            .counters
            .source_to_destination
            .payload_bytes
            .load(Ordering::Relaxed),
        5,
        "failure to begin a transaction must restore drained counters"
    );
}

#[test]
fn stream_counters_preserve_machine_direction() {
    let recorder = TrafficRecorder::default();
    let stream = recorder.register_machine_stream("workspace", "machine-a", "machine-b");
    stream.source_to_destination.record(11);
    stream.destination_to_source.record(7);
    stream.destination_to_source.record(5);

    assert_eq!(
        recorder.pending_for(
            "workspace",
            TrafficClass::VirtualNetwork,
            "machine-a",
            "machine-b"
        ),
        (11, 1)
    );
    assert_eq!(
        recorder.pending_for(
            "workspace",
            TrafficClass::VirtualNetwork,
            "machine-b",
            "machine-a"
        ),
        (12, 2)
    );
}

#[tokio::test]
async fn recent_includes_unflushed_counters() {
    let recorder = TrafficRecorder::default();
    let stream =
        recorder.register_client_stream("workspace", TrafficClass::ServiceIngress, "machine-a");
    stream.source_to_destination.record(11);
    stream.destination_to_source.record(7);

    let records = recorder
        .recent("workspace", 24)
        .await
        .expect("query pending traffic");
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|record| {
        record.source_server_id == "browser"
            && record.destination_server_id == "machine-a"
            && record.traffic_class == "service_ingress"
            && (
                record.payload_bytes,
                record.payload_frames,
                record.billable_bytes,
            ) == (11, 1, 11)
    }));
    assert!(records.iter().any(|record| {
        record.source_server_id == "machine-a"
            && record.destination_server_id == "browser"
            && record.traffic_class == "service_ingress"
            && (
                record.payload_bytes,
                record.payload_frames,
                record.billable_bytes,
            ) == (7, 1, 7)
    }));
}

#[tokio::test]
async fn usage_classes_do_not_share_a_billing_bucket() {
    let recorder = TrafficRecorder::default();
    recorder
        .register_client_stream("workspace", TrafficClass::ServiceIngress, "machine-a")
        .source_to_destination
        .record(11);
    recorder
        .register_client_stream("workspace", TrafficClass::AgentInterface, "machine-a")
        .source_to_destination
        .record(7);

    let records = recorder
        .recent("workspace", 1)
        .await
        .expect("query classified traffic");
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|record| {
        record.traffic_class == "service_ingress" && record.billable_bytes == 11
    }));
    assert!(records
        .iter()
        .any(|record| { record.traffic_class == "agent_interface" && record.billable_bytes == 7 }));
}

#[test]
fn pending_counters_merge_into_the_current_persisted_bucket() {
    let recorder = TrafficRecorder::default();
    let stream = recorder.register_machine_stream("workspace", "source", "destination");
    stream.source_to_destination.record(5);
    let window_start = hour_start(Utc::now().timestamp());
    let mut records = vec![MachineTrafficRecord {
        window_start: DateTime::from_timestamp(window_start, 0).expect("valid timestamp"),
        traffic_class: "virtual_network".to_string(),
        source_type: "machine".to_string(),
        source_server_id: "source".to_string(),
        destination_type: "machine".to_string(),
        destination_server_id: "destination".to_string(),
        payload_bytes: 12,
        payload_frames: 2,
        billable_bytes: 12,
        meter_version: 1,
    }];

    recorder
        .merge_pending("workspace", window_start, &mut records)
        .expect("merge pending traffic");

    assert_eq!(records.len(), 1);
    assert_eq!(
        (
            records[0].payload_bytes,
            records[0].payload_frames,
            records[0].billable_bytes
        ),
        (17, 3, 17)
    );
}

#[test]
fn traffic_window_contains_the_requested_number_of_hour_buckets() {
    let current_hour = 2_000 * 60 * 60;
    assert_eq!(traffic_window_start(current_hour, 1), current_hour);
    assert_eq!(
        traffic_window_start(current_hour + 37 * 60, 24),
        current_hour - 23 * 60 * 60
    );
}

#[tokio::test]
async fn flush_aggregates_counters_after_workspace_is_tombstoned() {
    let store = AuthStore::for_test("admin-password").await;
    store.seed_test_workspace("traffic").await;
    let recorder = TrafficRecorder::new(store.pool());
    let first = recorder.register_machine_stream("traffic", "source", "destination");
    first.source_to_destination.record(9);
    first.source_to_destination.record(3);
    recorder.flush_pending().await.expect("flush first batch");
    sqlx::query("UPDATE workspaces SET deleted_at = $1 WHERE workspace_id = $2")
        .bind(Utc::now().to_rfc3339())
        .bind("traffic")
        .execute(&store.pool())
        .await
        .expect("tombstone workspace");
    let second = recorder.register_machine_stream("traffic", "source", "destination");
    second.source_to_destination.record(5);
    second.destination_to_source.record(7);
    recorder.flush_pending().await.expect("flush second batch");

    let records = recorder.recent("traffic", 1).await.expect("query traffic");
    let outbound = records
        .iter()
        .find(|record| {
            record.source_server_id == "source" && record.destination_server_id == "destination"
        })
        .expect("outbound record");
    assert_eq!(
        (
            outbound.payload_bytes,
            outbound.payload_frames,
            outbound.billable_bytes
        ),
        (17, 3, 17)
    );
    let inbound = records
        .iter()
        .find(|record| {
            record.source_server_id == "destination" && record.destination_server_id == "source"
        })
        .expect("inbound record");
    assert_eq!(
        (
            inbound.payload_bytes,
            inbound.payload_frames,
            inbound.billable_bytes
        ),
        (7, 1, 7)
    );
}
