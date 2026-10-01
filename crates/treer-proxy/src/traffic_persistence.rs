use super::*;
use sqlx::{Postgres, QueryBuilder};

impl TrafficRecorder {
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
