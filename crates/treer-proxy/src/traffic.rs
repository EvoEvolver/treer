use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use tracing::warn;
use treer_protocol::{MachineTrafficRecord, NetworkUsageReport, NetworkUsageTotals, ProtocolError};

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
    pub(crate) async fn issue_usage_ticket(
        &self,
        workspace: &str,
        server: &str,
        agent: Option<&str>,
        host: &str,
        port: u16,
    ) -> anyhow::Result<Option<String>> {
        let Some(pool) = &self.inner.pool else {
            return Ok(None);
        };
        let ticket = uuid::Uuid::new_v4().to_string();
        let destination = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        sqlx::query("INSERT INTO network_usage_receipts (ticket,workspace_id,server_id,agent_id,destination,created_at) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(&ticket).bind(workspace).bind(server).bind(agent).bind(destination)
            .bind(Utc::now().timestamp()).execute(pool).await?;
        Ok(Some(ticket))
    }

    pub(crate) async fn abandon_usage_ticket(
        &self,
        workspace: &str,
        server: &str,
        ticket: &str,
    ) -> anyhow::Result<()> {
        let Some(pool) = &self.inner.pool else {
            return Ok(());
        };
        sqlx::query(
            "DELETE FROM network_usage_receipts \
             WHERE ticket=$1 AND workspace_id=$2 AND server_id=$3 \
             AND closed_at IS NULL AND sent_bytes=0 AND received_bytes=0 \
             AND sent_chunks=0 AND received_chunks=0",
        )
        .bind(ticket)
        .bind(workspace)
        .bind(server)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Commit deduplication state and both machine/Agent ledgers in one
    /// transaction. Only then may the Proxy acknowledge a replayable report.
    pub(crate) async fn persist_usage_report(
        &self,
        workspace: &str,
        server: &str,
        report: &NetworkUsageReport,
    ) -> anyhow::Result<()> {
        let pool = self
            .inner
            .pool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("usage receipt storage unavailable"))?;
        let values = [
            report.totals.sent_bytes,
            report.totals.received_bytes,
            report.totals.sent_chunks,
            report.totals.received_chunks,
        ];
        anyhow::ensure!(
            values.iter().all(|n| *n <= i64::MAX as u64),
            "usage counters exceed supported range"
        );
        let mut transaction = pool.begin().await?;
        let row = sqlx::query("SELECT agent_id,destination,sent_bytes,received_bytes,sent_chunks,received_chunks FROM network_usage_receipts WHERE ticket=$1 AND workspace_id=$2 AND server_id=$3 FOR UPDATE")
            .bind(&report.ticket).bind(workspace).bind(server).fetch_optional(&mut *transaction).await?
            .ok_or_else(|| anyhow::anyhow!("usage ticket does not belong to this machine/workspace"))?;
        let previous = [
            row.get::<i64, _>("sent_bytes"),
            row.get("received_bytes"),
            row.get("sent_chunks"),
            row.get("received_chunks"),
        ];
        if report.finished {
            sqlx::query("UPDATE network_usage_receipts SET closed_at=COALESCE(closed_at,$2) WHERE ticket=$1")
                .bind(&report.ticket).bind(Utc::now().timestamp()).execute(&mut *transaction).await?;
        }
        // Delayed retries may arrive after a newer cumulative report. They add
        // nothing; mixed increasing/decreasing directions are invalid.
        if values
            .iter()
            .zip(previous)
            .all(|(new, old)| *new <= old as u64)
        {
            transaction.commit().await?;
            return Ok(());
        }
        anyhow::ensure!(
            values
                .iter()
                .zip(previous)
                .all(|(new, old)| *new >= old as u64),
            "usage report moves counters backwards"
        );
        let deltas: Vec<_> = values
            .iter()
            .zip(previous)
            .map(|(new, old)| *new as i64 - old)
            .collect();
        let destination: String = row.get("destination");
        let agent: Option<String> = row.get("agent_id");
        let now = Utc::now();
        let window = now.timestamp().div_euclid(3600) * 3600;
        for (table, source_type, source_id) in [
            ("traffic_usage_hourly", "machine", Some(server)),
            ("agent_traffic_usage_hourly", "agent", agent.as_deref()),
        ] {
            let Some(source_id) = source_id else { continue };
            for (from_type, from_id, to_type, to_id, bytes, chunks) in [
                (
                    source_type,
                    source_id,
                    "internet",
                    destination.as_str(),
                    deltas[0],
                    deltas[2],
                ),
                (
                    "internet",
                    destination.as_str(),
                    source_type,
                    source_id,
                    deltas[1],
                    deltas[3],
                ),
            ] {
                if bytes == 0 && chunks == 0 {
                    continue;
                }
                // Table identifiers are static above, never supplied by a peer.
                let query = format!("INSERT INTO {table} (workspace_id,window_start,traffic_class,source_type,source_id,destination_type,destination_id,payload_bytes,payload_frames,billable_bytes,meter_version,updated_at) VALUES ($1,$2,'direct_network',$3,$4,$5,$6,$7,$8,0,2,$9) ON CONFLICT (workspace_id,window_start,traffic_class,source_type,source_id,destination_type,destination_id,meter_version) DO UPDATE SET payload_bytes={table}.payload_bytes+EXCLUDED.payload_bytes,payload_frames={table}.payload_frames+EXCLUDED.payload_frames,updated_at=EXCLUDED.updated_at");
                sqlx::query(&query)
                    .bind(workspace)
                    .bind(window)
                    .bind(from_type)
                    .bind(from_id)
                    .bind(to_type)
                    .bind(to_id)
                    .bind(bytes)
                    .bind(chunks)
                    .bind(now.to_rfc3339())
                    .execute(&mut *transaction)
                    .await?;
            }
        }
        sqlx::query("UPDATE network_usage_receipts SET sent_bytes=$2,received_bytes=$3,sent_chunks=$4,received_chunks=$5 WHERE ticket=$1")
            .bind(&report.ticket).bind(values[0] as i64).bind(values[1] as i64)
            .bind(values[2] as i64).bind(values[3] as i64).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

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
        let result = async {
            let mut transaction = pool.begin().await?;
            for chunk in deltas.chunks(MAX_ROWS_PER_INSERT) {
                let mut query = QueryBuilder::<Postgres>::new(self.sql(
                    "INSERT INTO traffic_usage_hourly(\
                     workspace_id, window_start, traffic_class, source_type, source_id, \
                     destination_type, destination_id, payload_bytes, payload_frames, \
                     billable_bytes, meter_version, updated_at) ",
                ));
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
                query.push(self.sql(
                    " ON CONFLICT(workspace_id, window_start, traffic_class, source_type, source_id, \
                     destination_type, destination_id, meter_version) DO UPDATE SET \
                     payload_bytes = traffic_usage_hourly.payload_bytes + EXCLUDED.payload_bytes, \
                     payload_frames = traffic_usage_hourly.payload_frames + EXCLUDED.payload_frames, \
                     billable_bytes = traffic_usage_hourly.billable_bytes + EXCLUDED.billable_bytes, \
                     updated_at = EXCLUDED.updated_at",
                ));
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
        sqlx::query(&self.sql("DELETE FROM traffic_usage_hourly WHERE window_start < $1"))
            .bind(cutoff)
            .execute(pool)
            .await?;
        if self.inner.agent_detail {
            return Ok(());
        }
        sqlx::query(
            "DELETE FROM network_usage_receipts \
             WHERE closed_at < $1 OR (closed_at IS NULL AND created_at < $1)",
        )
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
mod tests {
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
        let mut stream =
            recorder.register_direct_stream("workspace", "source", "example.test", 443);
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
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM network_usage_receipts WHERE ticket=$1"
            )
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
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM network_usage_receipts WHERE ticket=$1"
            )
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
