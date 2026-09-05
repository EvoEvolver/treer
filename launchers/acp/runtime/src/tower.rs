use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Url;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use uuid::Uuid;

const BATCH_SIZE: i64 = 128;

#[derive(Clone, Debug)]
pub struct TowerConfig {
    pub url: String,
    pub token: Option<String>,
    pub workspace_id: Option<String>,
}

struct TowerInner {
    connection: Mutex<Connection>,
    client: reqwest::Client,
    ingest_url: Url,
    token: Option<String>,
    collector_id: String,
    workspace_id: Option<String>,
    agent_id: String,
    notify: Notify,
}

#[derive(Clone)]
pub struct TowerCollector {
    inner: Arc<TowerInner>,
}

#[derive(Clone)]
pub struct TowerStream {
    inner: Arc<TowerInner>,
    stream_id: String,
}

impl TowerCollector {
    pub fn open(config: TowerConfig, state_dir: &Path, agent_id: &str) -> Result<Self> {
        let base = format!("{}/", config.url.trim_end_matches('/'));
        let ingest_url = Url::parse(&base)
            .context("parse TOWER_URL")?
            .join("v1/ingest")
            .context("construct TOWER ingest URL")?;
        let path = state_dir.join("tower-spool.sqlite");
        let connection = Connection::open(&path)
            .with_context(|| format!("open TOWER spool {}", path.display()))?;
        connection.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stream_heads (
                stream_id TEXT PRIMARY KEY,
                session_id TEXT,
                head_node_id TEXT,
                last_sequence INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
                local_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                stream_id TEXT NOT NULL,
                stream_sequence INTEGER NOT NULL,
                node_id TEXT NOT NULL,
                parent_id TEXT,
                payload_hash TEXT NOT NULL,
                payload TEXT NOT NULL,
                direction TEXT NOT NULL,
                method TEXT,
                rpc_id TEXT,
                occurred_at TEXT NOT NULL,
                uploaded INTEGER NOT NULL DEFAULT 0,
                UNIQUE(stream_id, stream_sequence)
            );
            CREATE INDEX IF NOT EXISTS tower_pending_events
                ON events(uploaded, local_sequence);
            ",
        )?;
        let collector_id = match connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'collector_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            Some(value) => value,
            None => {
                let value = format!("collector_{}", Uuid::new_v4().simple());
                connection.execute(
                    "INSERT INTO meta(key, value) VALUES('collector_id', ?1)",
                    params![value],
                )?;
                value
            }
        };
        let inner = Arc::new(TowerInner {
            connection: Mutex::new(connection),
            client: reqwest::Client::new(),
            ingest_url,
            token: config.token,
            collector_id,
            workspace_id: config.workspace_id,
            agent_id: agent_id.to_string(),
            notify: Notify::new(),
        });
        spawn_uploader(Arc::downgrade(&inner));
        inner.notify.notify_one();
        Ok(Self { inner })
    }

    pub fn start_stream(&self) -> Result<TowerStream> {
        let stream_id = format!("stream_{}", Uuid::new_v4().simple());
        self.inner
            .connection
            .lock()
            .expect("TOWER spool mutex")
            .execute(
                "INSERT INTO stream_heads(stream_id, last_sequence) VALUES(?1, 0)",
                params![stream_id],
            )?;
        Ok(TowerStream {
            inner: self.inner.clone(),
            stream_id,
        })
    }
}

impl TowerStream {
    pub fn record(&self, direction: &str, value: &Value) {
        if let Err(error) = self.try_record(direction, value) {
            tracing::warn!(%error, "failed to spool TOWER event");
        }
    }

    fn try_record(&self, direction: &str, value: &Value) -> Result<()> {
        let payload = serde_json::to_string(value)?;
        let payload_hash = digest(payload.as_bytes());
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string);
        let rpc_id = value.get("id").map(|value| match value {
            Value::String(value) => value.clone(),
            other => other.to_string(),
        });
        let session_id = session_id_from_payload(value);
        let mut connection = self.inner.connection.lock().expect("TOWER spool mutex");
        let transaction = connection.transaction()?;
        let (last_sequence, parent_id) = transaction.query_row(
            "SELECT last_sequence, head_node_id FROM stream_heads WHERE stream_id = ?1",
            params![self.stream_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )?;
        let stream_sequence = last_sequence + 1;
        let node_id = prefix_node_id(parent_id.as_deref(), direction, &payload_hash);
        let event_id = format!("event_{}", Uuid::new_v4().simple());
        transaction.execute(
            "INSERT INTO events(event_id, stream_id, stream_sequence, node_id, parent_id, payload_hash, payload, direction, method, rpc_id, occurred_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                event_id,
                self.stream_id,
                stream_sequence,
                node_id,
                parent_id,
                payload_hash,
                payload,
                direction,
                method,
                rpc_id,
                Utc::now().to_rfc3339(),
            ],
        )?;
        transaction.execute(
            "UPDATE stream_heads SET head_node_id = ?1, last_sequence = ?2, session_id = COALESCE(?3, session_id) WHERE stream_id = ?4",
            params![node_id, stream_sequence, session_id, self.stream_id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.inner.notify.notify_one();
        Ok(())
    }
}

fn session_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .pointer("/result/sessionId")
        .or_else(|| payload.pointer("/params/sessionId"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 240)
        .map(str::to_string)
}

fn prefix_node_id(parent_id: Option<&str>, direction: &str, payload_hash: &str) -> String {
    digest(format!("{}\n{direction}\n{payload_hash}", parent_id.unwrap_or("")).as_bytes())
}

fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn spawn_uploader(inner: Weak<TowerInner>) {
    tokio::spawn(async move {
        loop {
            let Some(current) = inner.upgrade() else {
                return;
            };
            tokio::select! {
                _ = current.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
            drop(current);
            // ACP updates often arrive in short bursts. Let the spool coalesce
            // them before draining so capture does not become one HTTP request per frame.
            tokio::time::sleep(Duration::from_millis(100)).await;
            while let Some(current) = inner.upgrade() {
                match pending_batch(&current) {
                    Ok(Some(batch)) => match upload_batch(&current, &batch).await {
                        Ok(()) => {
                            if let Err(error) = mark_uploaded(&current, &batch.event_ids) {
                                tracing::warn!(%error, "failed to commit TOWER upload receipt");
                                break;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "TOWER upload failed; events remain in local spool");
                            break;
                        }
                    },
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "failed to read TOWER spool");
                        break;
                    }
                }
            }
        }
    });
}

struct PendingBatch {
    body: Value,
    event_ids: Vec<String>,
}

fn pending_batch(inner: &TowerInner) -> Result<Option<PendingBatch>> {
    let connection = inner.connection.lock().expect("TOWER spool mutex");
    let stream_id = connection
        .query_row(
            "SELECT stream_id FROM events WHERE uploaded = 0 ORDER BY local_sequence LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(stream_id) = stream_id else {
        return Ok(None);
    };
    let session_id = connection.query_row(
        "SELECT session_id FROM stream_heads WHERE stream_id = ?1",
        params![stream_id],
        |row| row.get::<_, Option<String>>(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT event_id, stream_sequence, node_id, parent_id, payload_hash, payload, direction, method, rpc_id, occurred_at FROM events WHERE uploaded = 0 AND stream_id = ?1 ORDER BY stream_sequence LIMIT ?2",
    )?;
    let rows = statement.query_map(params![stream_id, BATCH_SIZE], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;
    let mut events = Vec::new();
    let mut event_ids = Vec::new();
    for row in rows {
        let (
            event_id,
            sequence,
            node_id,
            parent_id,
            payload_hash,
            payload,
            direction,
            method,
            rpc_id,
            occurred_at,
        ) = row?;
        event_ids.push(event_id.clone());
        events.push(json!({
            "event_id": event_id,
            "sequence": sequence,
            "node_id": node_id,
            "parent_id": parent_id,
            "payload_hash": payload_hash,
            "payload": serde_json::from_str::<Value>(&payload)?,
            "direction": direction,
            "method": method,
            "rpc_id": rpc_id,
            "occurred_at": occurred_at,
        }));
    }
    drop(statement);
    Ok(Some(PendingBatch {
        body: json!({
            "schema_version": 1,
            "stream": {
                "stream_id": stream_id,
                "collector_id": inner.collector_id,
                "workspace_id": inner.workspace_id,
                "agent_id": inner.agent_id,
                "session_id": session_id,
            },
            "events": events,
        }),
        event_ids,
    }))
}

async fn upload_batch(inner: &TowerInner, batch: &PendingBatch) -> Result<()> {
    let mut request = inner
        .client
        .post(inner.ingest_url.clone())
        .json(&batch.body);
    if let Some(token) = &inner.token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.context("send TOWER batch")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("TOWER rejected batch with {status}: {body}");
    }
    Ok(())
}

fn mark_uploaded(inner: &TowerInner, event_ids: &[String]) -> Result<()> {
    let mut connection = inner.connection.lock().expect("TOWER spool mutex");
    let transaction = connection.transaction()?;
    for event_id in event_ids {
        transaction.execute(
            "UPDATE events SET uploaded = 1 WHERE event_id = ?1",
            params![event_id],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn canonical_hash_matches_the_tower_app_contract() {
        let value = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        let payload = serde_json::to_string(&value).unwrap();
        let payload_hash = digest(payload.as_bytes());
        assert_eq!(
            payload_hash,
            "2228a4133e56a504b4e3654d1120b4f4adc9a266ec4af9d10cc9958e087ae348"
        );
        assert_eq!(
            prefix_node_id(None, "client_to_agent", &payload_hash),
            "b47d02ad89b92da0dba86c22957d656a6982825085e60265ca02f2231b9f44d7"
        );
    }

    #[tokio::test]
    async fn spools_a_prefix_chain_without_copying_history() {
        let dir = tempdir().unwrap();
        let collector = TowerCollector::open(
            TowerConfig {
                url: "http://127.0.0.1:1/base/".into(),
                token: None,
                workspace_id: Some("workspace_test".into()),
            },
            dir.path(),
            "agent_test",
        )
        .unwrap();
        let stream = collector.start_stream().unwrap();
        stream.record(
            "client_to_agent",
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        );
        stream.record(
            "agent_to_client",
            &json!({"jsonrpc":"2.0","id":1,"result":{}}),
        );

        let connection = collector.inner.connection.lock().unwrap();
        let rows = connection
            .prepare("SELECT stream_sequence, parent_id, node_id, payload FROM events ORDER BY stream_sequence")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].1.is_none());
        assert_eq!(rows[1].1.as_deref(), Some(rows[0].2.as_str()));
        assert!(!rows[1].3.contains("initialize"));
    }
}
