use super::*;
use sqlx::{Postgres, QueryBuilder};

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
    pub(crate) async fn delete_expired(&self) -> anyhow::Result<()> {
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
}
