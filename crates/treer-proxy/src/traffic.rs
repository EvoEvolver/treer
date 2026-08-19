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

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct TrafficKey {
    workspace_id: String,
    source_server_id: String,
    destination_server_id: String,
}

#[derive(Debug, Default)]
pub(crate) struct TrafficCounter {
    payload_bytes: AtomicU64,
    payload_frames: AtomicU64,
}

impl TrafficCounter {
    pub(crate) fn record(&self, payload_bytes: usize) {
        self.payload_bytes.fetch_add(
            u64::try_from(payload_bytes).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.payload_frames.fetch_add(1, Ordering::Relaxed);
    }
}

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
}

struct TrafficDelta {
    key: TrafficKey,
    payload_bytes: u64,
    payload_frames: u64,
}

impl TrafficRecorder {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            inner: Arc::new(TrafficRecorderInner {
                pool: Some(pool),
                counters: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn register_stream(
        &self,
        workspace_id: &str,
        source_server_id: &str,
        destination_server_id: &str,
    ) -> StreamTrafficCounters {
        let mut counters = self.counters();
        StreamTrafficCounters {
            source_to_destination: counter_for(
                &mut counters,
                workspace_id,
                source_server_id,
                destination_server_id,
            ),
            destination_to_source: counter_for(
                &mut counters,
                workspace_id,
                destination_server_id,
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
                            warn!(%error, "failed to persist machine traffic counters");
                        }
                    }
                    _ = cleanup.tick() => {
                        if let Err(error) = recorder.delete_expired().await {
                            warn!(%error, "failed to delete expired machine traffic counters");
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
        let Some(pool) = &self.inner.pool else {
            return Ok(Vec::new());
        };
        let cutoff = Utc::now()
            .timestamp()
            .saturating_sub(i64::from(hours) * 60 * 60);
        let rows = sqlx::query(
            "SELECT window_start, source_server_id, destination_server_id, payload_bytes, \
             payload_frames FROM machine_traffic_hourly WHERE workspace_id = $1 \
             AND window_start >= $2 ORDER BY window_start DESC, source_server_id, destination_server_id",
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
                    source_server_id: row.get("source_server_id"),
                    destination_server_id: row.get("destination_server_id"),
                    payload_bytes: database_counter(&row, "payload_bytes")?,
                    payload_frames: database_counter(&row, "payload_frames")?,
                })
            })
            .collect()
    }

    async fn flush_pending(&self) -> anyhow::Result<()> {
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
                    "INSERT INTO machine_traffic_hourly(\
                     workspace_id, window_start, source_server_id, destination_server_id, \
                     payload_bytes, payload_frames, updated_at) ",
                );
                query.push_values(chunk, |mut row, delta| {
                    row.push_bind(&delta.key.workspace_id)
                        .push_bind(window_start)
                        .push_bind(&delta.key.source_server_id)
                        .push_bind(&delta.key.destination_server_id)
                        .push_bind(database_value(delta.payload_bytes))
                        .push_bind(database_value(delta.payload_frames))
                        .push_bind(&updated_at);
                });
                query.push(
                    " ON CONFLICT(workspace_id, window_start, source_server_id, destination_server_id) \
                     DO UPDATE SET payload_bytes = machine_traffic_hourly.payload_bytes + EXCLUDED.payload_bytes, \
                     payload_frames = machine_traffic_hourly.payload_frames + EXCLUDED.payload_frames, \
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

    async fn delete_expired(&self) -> anyhow::Result<()> {
        let Some(pool) = &self.inner.pool else {
            return Ok(());
        };
        let cutoff = Utc::now()
            .timestamp()
            .saturating_sub(i64::try_from(RETENTION.as_secs()).unwrap_or(i64::MAX));
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
                (payload_bytes != 0 || payload_frames != 0).then(|| TrafficDelta {
                    key: key.clone(),
                    payload_bytes,
                    payload_frames,
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
            }
        }
    }

    fn prune_idle(&self) {
        self.counters().retain(|_, counter| {
            Arc::strong_count(counter) > 1
                || counter.payload_bytes.load(Ordering::Relaxed) != 0
                || counter.payload_frames.load(Ordering::Relaxed) != 0
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
        source_server_id: &str,
        destination_server_id: &str,
    ) -> (u64, u64) {
        self.counters()
            .get(&TrafficKey {
                workspace_id: workspace_id.to_string(),
                source_server_id: source_server_id.to_string(),
                destination_server_id: destination_server_id.to_string(),
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
    source_server_id: &str,
    destination_server_id: &str,
) -> Arc<TrafficCounter> {
    counters
        .entry(TrafficKey {
            workspace_id: workspace_id.to_string(),
            source_server_id: source_server_id.to_string(),
            destination_server_id: destination_server_id.to_string(),
        })
        .or_default()
        .clone()
}

fn hour_start(timestamp: i64) -> i64 {
    timestamp - timestamp.rem_euclid(60 * 60)
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
        let stream = recorder.register_stream("workspace", "machine-a", "machine-b");
        stream.source_to_destination.record(11);
        stream.destination_to_source.record(7);
        stream.destination_to_source.record(5);

        assert_eq!(
            recorder.pending_for("workspace", "machine-a", "machine-b"),
            (11, 1)
        );
        assert_eq!(
            recorder.pending_for("workspace", "machine-b", "machine-a"),
            (12, 2)
        );
    }

    #[tokio::test]
    async fn flush_aggregates_directional_counters_in_postgres() {
        let store = AuthStore::for_test("admin-password").await;
        store.seed_test_workspace("traffic").await;
        let recorder = TrafficRecorder::new(store.database_pool());
        let first = recorder.register_stream("traffic", "source", "destination");
        first.source_to_destination.record(9);
        first.source_to_destination.record(3);
        recorder.flush_pending().await.expect("flush first batch");
        let second = recorder.register_stream("traffic", "source", "destination");
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
        assert_eq!((outbound.payload_bytes, outbound.payload_frames), (17, 3));
        let inbound = records
            .iter()
            .find(|record| {
                record.source_server_id == "destination" && record.destination_server_id == "source"
            })
            .expect("inbound record");
        assert_eq!((inbound.payload_bytes, inbound.payload_frames), (7, 1));
    }
}
