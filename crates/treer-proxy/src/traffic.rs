use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use tracing::warn;
use treer_protocol::{MachineTrafficRecord, NetworkUsageReport, NetworkUsageTotals, ProtocolError};

#[path = "traffic_persistence.rs"]
mod persistence;

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
    DirectNetwork,
}

impl TrafficClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::VirtualNetwork => "virtual_network",
            Self::ServiceIngress => "service_ingress",
            Self::VirtualHost => "virtual_host",
            Self::AgentInterface => "agent_interface",
            Self::DirectNetwork => "direct_network",
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
    fn record_observed(&self, bytes: u64, chunks: u64) {
        self.payload_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.payload_frames.fetch_add(chunks, Ordering::Relaxed);
        // Controller-reported internet writes are observation, not relay billing.
    }
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

pub(crate) struct DirectTrafficMeter {
    counters: StreamTrafficCounters,
    agent_counters: Option<StreamTrafficCounters>,
    previous: NetworkUsageTotals,
}

impl DirectTrafficMeter {
    pub(crate) fn with_agent_meter(mut self, meter: Option<Self>) -> Self {
        self.agent_counters = meter.map(|meter| meter.counters);
        self
    }
    pub(crate) fn record(&mut self, totals: NetworkUsageTotals) -> Result<(), ProtocolError> {
        let previous = self.previous;
        if totals.sent_bytes < previous.sent_bytes
            || totals.received_bytes < previous.received_bytes
            || totals.sent_chunks < previous.sent_chunks
            || totals.received_chunks < previous.received_chunks
        {
            return Err(ProtocolError::new(
                "invalid_network_usage",
                "cumulative network usage cannot decrease",
            ));
        }
        if [
            totals.sent_bytes,
            totals.received_bytes,
            totals.sent_chunks,
            totals.received_chunks,
        ]
        .iter()
        .any(|value| *value > i64::MAX as u64)
        {
            return Err(ProtocolError::new(
                "invalid_network_usage",
                "network usage exceeds supported counters",
            ));
        }
        self.counters.source_to_destination.record_observed(
            totals.sent_bytes - previous.sent_bytes,
            totals.sent_chunks - previous.sent_chunks,
        );
        self.counters.destination_to_source.record_observed(
            totals.received_bytes - previous.received_bytes,
            totals.received_chunks - previous.received_chunks,
        );
        self.previous = totals;
        if let Some(counters) = &self.agent_counters {
            counters.source_to_destination.record_observed(
                totals.sent_bytes - previous.sent_bytes,
                totals.sent_chunks - previous.sent_chunks,
            );
            counters.destination_to_source.record_observed(
                totals.received_bytes - previous.received_bytes,
                totals.received_chunks - previous.received_chunks,
            );
        }
        Ok(())
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
    agent_detail: bool,
    agents: OnceLock<TrafficRecorder>,
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

    pub(crate) fn agent_view(&self) -> Self {
        if self.inner.agent_detail {
            return self.clone();
        }
        self.inner
            .agents
            .get_or_init(|| Self {
                inner: Arc::new(TrafficRecorderInner {
                    pool: self.inner.pool.clone(),
                    agent_detail: true,
                    ..TrafficRecorderInner::default()
                }),
            })
            .clone()
    }

    fn sql(&self, sql: &str) -> String {
        if self.inner.agent_detail {
            sql.replace("traffic_usage_hourly", "agent_traffic_usage_hourly")
                .replace(
                    "FROM machine_traffic_hourly WHERE",
                    "FROM machine_traffic_hourly WHERE FALSE AND",
                )
        } else {
            sql.to_string()
        }
    }

    pub(crate) fn register_agent_stream(
        &self,
        workspace: &str,
        source_server: &str,
        destination_server: &str,
        source_agent: Option<&str>,
        destination_agent: Option<&str>,
    ) -> Option<StreamTrafficCounters> {
        if source_agent.is_none() && destination_agent.is_none() {
            return None;
        }
        Some(self.register_stream(
            workspace,
            TrafficClass::VirtualNetwork,
            if source_agent.is_some() {
                "agent"
            } else {
                ENDPOINT_MACHINE
            },
            source_agent.unwrap_or(source_server),
            if destination_agent.is_some() {
                "agent"
            } else {
                ENDPOINT_MACHINE
            },
            destination_agent.unwrap_or(destination_server),
        ))
    }
    pub(crate) fn register_direct_stream(
        &self,
        workspace_id: &str,
        source_server_id: &str,
        host: &str,
        port: u16,
    ) -> DirectTrafficMeter {
        let destination = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        DirectTrafficMeter {
            counters: self.register_stream(
                workspace_id,
                TrafficClass::DirectNetwork,
                if self.inner.agent_detail {
                    "agent"
                } else {
                    ENDPOINT_MACHINE
                },
                source_server_id,
                "internet",
                &destination,
            ),
            previous: NetworkUsageTotals::default(),
            agent_counters: None,
        }
    }
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            inner: Arc::new(TrafficRecorderInner {
                pool: Some(pool),
                counters: Mutex::new(HashMap::new()),
                flush_gate: tokio::sync::Mutex::new(()),
                agent_detail: false,
                agents: OnceLock::new(),
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
        if !self.inner.agent_detail {
            self.agent_view().spawn_flush_task();
        }
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
            let statement = self.sql(
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
            );
            let rows = sqlx::query(&statement)
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
            meter_version: if traffic_class == TrafficClass::DirectNetwork {
                2
            } else {
                METER_VERSION
            },
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
#[path = "traffic_tests.rs"]
mod tests;
