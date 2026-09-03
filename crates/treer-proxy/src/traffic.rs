use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use tracing::warn;
use treer_protocol::MachineTrafficRecord;

const FLUSH_INTERVAL: Duration = Duration::from_secs(10);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const RETENTION: Duration = Duration::from_secs(90 * 24 * 60 * 60);
const MAX_ROWS_PER_INSERT: usize = 500;
const METER_VERSION: u16 = 1;
pub(crate) const BROWSER_TRAFFIC_ENDPOINT: &str = "browser";
const ENDPOINT_CLIENT: &str = "client";
const ENDPOINT_MACHINE: &str = "machine";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrafficClass {
    VirtualNetwork,
    ServiceIngress,
    VirtualHost,
    AgentInterface,
}

impl TrafficClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::VirtualNetwork => "virtual_network",
            Self::ServiceIngress => "service_ingress",
            Self::VirtualHost => "virtual_host",
            Self::AgentInterface => "agent_interface",
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct TrafficKey {
    workspace_id: String,
    traffic_class: String,
    source_type: String,
    source_server_id: String,
    destination_type: String,
    destination_server_id: String,
    meter_version: u16,
}

#[derive(Debug, Default)]
pub(crate) struct TrafficCounter {
    payload_bytes: AtomicU64,
    payload_frames: AtomicU64,
    billable_bytes: AtomicU64,
}

impl TrafficCounter {
    pub(crate) fn record(&self, payload_bytes: usize) {
        self.payload_bytes.fetch_add(
            u64::try_from(payload_bytes).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.payload_frames.fetch_add(1, Ordering::Relaxed);
        self.billable_bytes.fetch_add(
            u64::try_from(payload_bytes).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

#[derive(Clone)]
pub(crate) struct StreamTrafficCounters {
    pub source_to_destination: Arc<TrafficCounter>,
    pub destination_to_source: Arc<TrafficCounter>,
}

#[derive(Clone, Default)]
pub(crate) struct TrafficRecorder {
    inner: Arc<TrafficRecorderInner>,
}

#[derive(Default)]
struct TrafficRecorderInner {
    pool: Option<PgPool>,
    counters: Mutex<HashMap<TrafficKey, Arc<TrafficCounter>>>,
    flush_gate: tokio::sync::Mutex<()>,
}

struct TrafficDelta {
    key: TrafficKey,
    payload_bytes: u64,
    payload_frames: u64,
    billable_bytes: u64,
}

impl TrafficRecorder {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            inner: Arc::new(TrafficRecorderInner {
                pool: Some(pool),
                counters: Mutex::new(HashMap::new()),
                flush_gate: tokio::sync::Mutex::new(()),
            }),
        }
    }

    pub(crate) fn register_machine_stream(
        &self,
        workspace_id: &str,
        source_server_id: &str,
        destination_server_id: &str,
    ) -> StreamTrafficCounters {
        self.register_stream(
            workspace_id,
            TrafficClass::VirtualNetwork,
            ENDPOINT_MACHINE,
            source_server_id,
            ENDPOINT_MACHINE,
            destination_server_id,
        )
    }

    pub(crate) fn register_client_stream(
        &self,
        workspace_id: &str,
        traffic_class: TrafficClass,
        destination_server_id: &str,
    ) -> StreamTrafficCounters {
        debug_assert_ne!(traffic_class, TrafficClass::VirtualNetwork);
        self.register_stream(
            workspace_id,
            traffic_class,
            ENDPOINT_CLIENT,
            BROWSER_TRAFFIC_ENDPOINT,
            ENDPOINT_MACHINE,
            destination_server_id,
        )
    }

    fn register_stream(
        &self,
        workspace_id: &str,
        traffic_class: TrafficClass,
        source_type: &str,
        source_server_id: &str,
        destination_type: &str,
        destination_server_id: &str,
    ) -> StreamTrafficCounters {
        let mut counters = self.counters();
        StreamTrafficCounters {
            source_to_destination: counter_for(
                &mut counters,
                workspace_id,
                traffic_class,
                source_type,
                source_server_id,
                destination_type,
                destination_server_id,
            ),
            destination_to_source: counter_for(
                &mut counters,
                workspace_id,
                traffic_class,
                destination_type,
                destination_server_id,
                source_type,
                source_server_id,
            ),
        }
    }

    pub(crate) fn spawn_flush_task(&self) {
        let recorder = self.clone();
        tokio::spawn(async move {
            let mut flush = tokio::time::interval(FLUSH_INTERVAL);
            let mut cleanup = tokio::time::interval(CLEANUP_INTERVAL);
            flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            cleanup.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            flush.tick().await;
            cleanup.tick().await;
            loop {
                tokio::select! {
                    _ = flush.tick() => {
                        if let Err(error) = recorder.flush_pending().await {
                            warn!(%error, "failed to persist traffic usage counters");
                        }
                    }
                    _ = cleanup.tick() => {
                        if let Err(error) = recorder.delete_expired().await {
                            warn!(%error, "failed to delete expired traffic usage counters");
                        }
                    }
                }
            }
        });
    }

    pub(crate) async fn recent(
        &self,
        workspace_id: &str,
        hours: u16,
    ) -> anyhow::Result<Vec<MachineTrafficRecord>> {
        let _flush = self.inner.flush_gate.lock().await;
        let now = Utc::now().timestamp();
        let cutoff = traffic_window_start(now, hours);
        let mut records = if let Some(pool) = &self.inner.pool {
            let rows = sqlx::query(
                "SELECT window_start, traffic_class, source_type, source_id, destination_type, \
                 destination_id, CAST(SUM(payload_bytes) AS BIGINT) AS payload_bytes, \
                 CAST(SUM(payload_frames) AS BIGINT) AS payload_frames, \
                 CAST(SUM(billable_bytes) AS BIGINT) AS billable_bytes, \
                 meter_version FROM (\
                   SELECT window_start, traffic_class, source_type, source_id, destination_type, \
                     destination_id, payload_bytes, payload_frames, billable_bytes, meter_version \
                   FROM traffic_usage_hourly WHERE workspace_id = $1 AND window_start >= $2 \
                   UNION ALL \
                   SELECT window_start, 'virtual_network', 'machine', source_server_id, 'machine', \
                     destination_server_id, payload_bytes, payload_frames, payload_bytes, 1 \
                   FROM machine_traffic_hourly WHERE workspace_id = $1 AND window_start >= $2\
                 ) AS usage GROUP BY window_start, traffic_class, source_type, source_id, \
                 destination_type, destination_id, meter_version \
                 ORDER BY window_start DESC, source_id, destination_id",
            )
            .bind(workspace_id)
            .bind(cutoff)
            .fetch_all(pool)
            .await?;
            rows.into_iter()
                .map(|row| {
                    let timestamp = row.get::<i64, _>("window_start");
                    let window_start = DateTime::from_timestamp(timestamp, 0)
                        .ok_or_else(|| anyhow::anyhow!("traffic row has invalid window_start"))?;
                    Ok(MachineTrafficRecord {
                        window_start,
                        traffic_class: row.get("traffic_class"),
                        source_type: row.get("source_type"),
                        source_server_id: row.get("source_id"),
                        destination_type: row.get("destination_type"),
                        destination_server_id: row.get("destination_id"),
                        payload_bytes: database_counter(&row, "payload_bytes")?,
                        payload_frames: database_counter(&row, "payload_frames")?,
                        billable_bytes: database_counter(&row, "billable_bytes")?,
                        meter_version: u16::try_from(row.get::<i32, _>("meter_version")).map_err(
                            |_| anyhow::anyhow!("traffic row has invalid meter_version"),
                        )?,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        self.merge_pending(workspace_id, hour_start(now), &mut records)?;
        records.sort_by(|left, right| {
            right
                .window_start
                .cmp(&left.window_start)
                .then_with(|| left.source_server_id.cmp(&right.source_server_id))
                .then_with(|| left.destination_server_id.cmp(&right.destination_server_id))
                .then_with(|| left.traffic_class.cmp(&right.traffic_class))
        });
        Ok(records)
    }

    pub(crate) async fn flush_pending(&self) -> anyhow::Result<()> {
        let _flush = self.inner.flush_gate.lock().await;
        let Some(pool) = &self.inner.pool else {
            return Ok(());
        };
        let deltas = self.take_pending();
        if deltas.is_empty() {
            self.prune_idle();
            return Ok(());
        }
        let window_start = hour_start(Utc::now().timestamp());
        let updated_at = Utc::now().to_rfc3339();
        let mut transaction = pool.begin().await?;
        let result = async {
            for chunk in deltas.chunks(MAX_ROWS_PER_INSERT) {
                let mut query = QueryBuilder::<Postgres>::new(
                    "INSERT INTO traffic_usage_hourly(\
                     workspace_id, window_start, traffic_class, source_type, source_id, \
                     destination_type, destination_id, payload_bytes, payload_frames, \
                     billable_bytes, meter_version, updated_at) ",
                );
                query.push_values(chunk, |mut row, delta| {
                    row.push_bind(&delta.key.workspace_id)
                        .push_bind(window_start)
                        .push_bind(&delta.key.traffic_class)
                        .push_bind(&delta.key.source_type)
                        .push_bind(&delta.key.source_server_id)
                        .push_bind(&delta.key.destination_type)
                        .push_bind(&delta.key.destination_server_id)
                        .push_bind(database_value(delta.payload_bytes))
                        .push_bind(database_value(delta.payload_frames))
                        .push_bind(database_value(delta.billable_bytes))
                        .push_bind(i32::from(delta.key.meter_version))
                        .push_bind(&updated_at);
                });
                query.push(
                    " ON CONFLICT(workspace_id, window_start, traffic_class, source_type, source_id, \
                     destination_type, destination_id, meter_version) DO UPDATE SET \
                     payload_bytes = traffic_usage_hourly.payload_bytes + EXCLUDED.payload_bytes, \
                     payload_frames = traffic_usage_hourly.payload_frames + EXCLUDED.payload_frames, \
                     billable_bytes = traffic_usage_hourly.billable_bytes + EXCLUDED.billable_bytes, \
                     updated_at = EXCLUDED.updated_at",
                );
                query.build().execute(&mut *transaction).await?;
            }
            transaction.commit().await
        }
        .await;
        if let Err(error) = result {
            self.restore(&deltas);
            return Err(error.into());
        }
        self.prune_idle();
        Ok(())
    }

    fn merge_pending(
        &self,
        workspace_id: &str,
        window_start: i64,
        records: &mut Vec<MachineTrafficRecord>,
    ) -> anyhow::Result<()> {
        let window = DateTime::from_timestamp(window_start, 0)
            .ok_or_else(|| anyhow::anyhow!("traffic window timestamp is invalid"))?;
        for (key, counter) in self.counters().iter() {
            if key.workspace_id != workspace_id {
                continue;
            }
            let payload_bytes = counter.payload_bytes.load(Ordering::Relaxed);
            let payload_frames = counter.payload_frames.load(Ordering::Relaxed);
            let billable_bytes = counter.billable_bytes.load(Ordering::Relaxed);
            if payload_bytes == 0 && payload_frames == 0 && billable_bytes == 0 {
                continue;
            }
            if let Some(record) = records.iter_mut().find(|record| {
                record.window_start == window
                    && record.traffic_class == key.traffic_class
                    && record.source_type == key.source_type
                    && record.source_server_id == key.source_server_id
                    && record.destination_type == key.destination_type
                    && record.destination_server_id == key.destination_server_id
                    && record.meter_version == key.meter_version
            }) {
                record.payload_bytes = record.payload_bytes.saturating_add(payload_bytes);
                record.payload_frames = record.payload_frames.saturating_add(payload_frames);
                record.billable_bytes = record.billable_bytes.saturating_add(billable_bytes);
            } else {
                records.push(MachineTrafficRecord {
                    window_start: window,
                    traffic_class: key.traffic_class.clone(),
                    source_type: key.source_type.clone(),
                    source_server_id: key.source_server_id.clone(),
                    destination_type: key.destination_type.clone(),
                    destination_server_id: key.destination_server_id.clone(),
                    payload_bytes,
                    payload_frames,
                    billable_bytes,
                    meter_version: key.meter_version,
                });
            }
        }
        Ok(())
    }

    async fn delete_expired(&self) -> anyhow::Result<()> {
        let Some(pool) = &self.inner.pool else {
            return Ok(());
        };
        let cutoff = Utc::now()
            .timestamp()
            .saturating_sub(i64::try_from(RETENTION.as_secs()).unwrap_or(i64::MAX));
        sqlx::query("DELETE FROM traffic_usage_hourly WHERE window_start < $1")
            .bind(cutoff)
            .execute(pool)
            .await?;
        sqlx::query("DELETE FROM machine_traffic_hourly WHERE window_start < $1")
            .bind(cutoff)
            .execute(pool)
            .await?;
        Ok(())
    }

    fn take_pending(&self) -> Vec<TrafficDelta> {
        self.counters()
            .iter()
            .filter_map(|(key, counter)| {
                let payload_bytes = counter.payload_bytes.swap(0, Ordering::Relaxed);
                let payload_frames = counter.payload_frames.swap(0, Ordering::Relaxed);
                let billable_bytes = counter.billable_bytes.swap(0, Ordering::Relaxed);
                (payload_bytes != 0 || payload_frames != 0 || billable_bytes != 0).then(|| {
                    TrafficDelta {
                        key: key.clone(),
                        payload_bytes,
                        payload_frames,
                        billable_bytes,
                    }
                })
            })
            .collect()
    }

    fn restore(&self, deltas: &[TrafficDelta]) {
        let counters = self.counters();
        for delta in deltas {
            if let Some(counter) = counters.get(&delta.key) {
                counter
                    .payload_bytes
                    .fetch_add(delta.payload_bytes, Ordering::Relaxed);
                counter
                    .payload_frames
                    .fetch_add(delta.payload_frames, Ordering::Relaxed);
                counter
                    .billable_bytes
                    .fetch_add(delta.billable_bytes, Ordering::Relaxed);
            }
        }
    }

    fn prune_idle(&self) {
        self.counters().retain(|_, counter| {
            Arc::strong_count(counter) > 1
                || counter.payload_bytes.load(Ordering::Relaxed) != 0
                || counter.payload_frames.load(Ordering::Relaxed) != 0
                || counter.billable_bytes.load(Ordering::Relaxed) != 0
        });
    }

    fn counters(&self) -> MutexGuard<'_, HashMap<TrafficKey, Arc<TrafficCounter>>> {
        self.inner
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn pending_for(
        &self,
        workspace_id: &str,
        traffic_class: TrafficClass,
        source_server_id: &str,
        destination_server_id: &str,
    ) -> (u64, u64) {
        self.counters()
            .get(&TrafficKey {
                workspace_id: workspace_id.to_string(),
                traffic_class: traffic_class.as_str().to_string(),
                source_type: if source_server_id == BROWSER_TRAFFIC_ENDPOINT {
                    ENDPOINT_CLIENT
                } else {
                    ENDPOINT_MACHINE
                }
                .to_string(),
                source_server_id: source_server_id.to_string(),
                destination_type: if destination_server_id == BROWSER_TRAFFIC_ENDPOINT {
                    ENDPOINT_CLIENT
                } else {
                    ENDPOINT_MACHINE
                }
                .to_string(),
                destination_server_id: destination_server_id.to_string(),
                meter_version: METER_VERSION,
            })
            .map_or((0, 0), |counter| {
                (
                    counter.payload_bytes.load(Ordering::Relaxed),
                    counter.payload_frames.load(Ordering::Relaxed),
                )
            })
    }
}

fn counter_for(
    counters: &mut HashMap<TrafficKey, Arc<TrafficCounter>>,
    workspace_id: &str,
    traffic_class: TrafficClass,
    source_type: &str,
    source_server_id: &str,
    destination_type: &str,
    destination_server_id: &str,
) -> Arc<TrafficCounter> {
    counters
        .entry(TrafficKey {
            workspace_id: workspace_id.to_string(),
            traffic_class: traffic_class.as_str().to_string(),
            source_type: source_type.to_string(),
            source_server_id: source_server_id.to_string(),
            destination_type: destination_type.to_string(),
            destination_server_id: destination_server_id.to_string(),
            meter_version: METER_VERSION,
        })
        .or_default()
        .clone()
}

fn hour_start(timestamp: i64) -> i64 {
    timestamp - timestamp.rem_euclid(60 * 60)
}

fn traffic_window_start(timestamp: i64, hours: u16) -> i64 {
    hour_start(timestamp).saturating_sub(i64::from(hours.saturating_sub(1)) * 60 * 60)
}

fn database_value(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn database_counter(row: &sqlx::postgres::PgRow, column: &str) -> anyhow::Result<u64> {
    u64::try_from(row.get::<i64, _>(column))
        .map_err(|_| anyhow::anyhow!("traffic row has invalid {column}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthStore;

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
        assert!(records.iter().any(|record| {
            record.traffic_class == "agent_interface" && record.billable_bytes == 7
        }));
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
}
