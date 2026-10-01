use super::*;
use crate::auth::AuthStore;

#[test]
fn stream_counters_preserve_machine_direction() {
    let recorder = TrafficRecorder::default();
    let stream = recorder.register_stream(
        "workspace",
        TrafficClass::VirtualNetwork,
        ENDPOINT_MACHINE,
        "machine-a",
        ENDPOINT_MACHINE,
        "machine-b",
    );
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
    let stream = recorder.register_stream(
        "workspace",
        TrafficClass::VirtualNetwork,
        ENDPOINT_MACHINE,
        "source",
        ENDPOINT_MACHINE,
        "destination",
    );
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
    let first = recorder.register_stream(
        "traffic",
        TrafficClass::VirtualNetwork,
        ENDPOINT_MACHINE,
        "source",
        ENDPOINT_MACHINE,
        "destination",
    );
    first.source_to_destination.record(9);
    first.source_to_destination.record(3);
    recorder.flush_pending().await.expect("flush first batch");
    sqlx::query("UPDATE workspaces SET deleted_at = $1 WHERE workspace_id = $2")
        .bind(Utc::now().to_rfc3339())
        .bind("traffic")
        .execute(&store.pool())
        .await
        .expect("tombstone workspace");
    let second = recorder.register_stream(
        "traffic",
        TrafficClass::VirtualNetwork,
        ENDPOINT_MACHINE,
        "source",
        ENDPOINT_MACHINE,
        "destination",
    );
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
