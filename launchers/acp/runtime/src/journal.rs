use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use treer_protocol::AgentTranscriptEntry;

use crate::types::{now_rfc3339, BoundSession, HistoryItem};

pub struct Journal {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("open journal {}", path.display()))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS entries (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                role TEXT,
                content TEXT NOT NULL,
                created_at TEXT
            );
            CREATE TABLE IF NOT EXISTS operations (
                operation_id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS kv (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            ",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn upsert_entry(&self, entry: &AgentTranscriptEntry) -> Result<()> {
        let conn = self.conn.lock().expect("journal mutex");
        let content = serde_json::to_string(&entry.content)?;
        conn.execute(
            "INSERT INTO entries(id, kind, role, content, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                kind=excluded.kind,
                role=excluded.role,
                content=excluded.content",
            params![entry.id, entry.kind, entry.role, content, entry.created_at],
        )?;
        Ok(())
    }

    pub fn entries(&self) -> Result<Vec<AgentTranscriptEntry>> {
        let conn = self.conn.lock().expect("journal mutex");
        let mut stmt = conn
            .prepare("SELECT id, kind, role, content, created_at FROM entries ORDER BY seq ASC")?;
        let rows = stmt.query_map([], |row| {
            let content: String = row.get(3)?;
            Ok(AgentTranscriptEntry {
                id: row.get(0)?,
                kind: row.get(1)?,
                role: row.get(2)?,
                content: serde_json::from_str(&content).unwrap_or(serde_json::Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn claim_operation(&self, operation_id: &str) -> Result<bool> {
        let conn = self.conn.lock().expect("journal mutex");
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO operations(operation_id, created_at) VALUES(?1, ?2)",
            params![operation_id, now_rfc3339()],
        )?;
        Ok(inserted == 0)
    }

    pub fn bind_session(
        &self,
        harness: &str,
        session_id: &str,
        cwd: &Path,
    ) -> Result<BoundSession> {
        let conn = self.conn.lock().expect("journal mutex");
        conn.execute(
            "INSERT INTO kv(key, value) VALUES('harness', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![harness],
        )?;
        conn.execute(
            "INSERT INTO kv(key, value) VALUES('session_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![session_id],
        )?;
        conn.execute(
            "INSERT INTO kv(key, value) VALUES('cwd', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![cwd.to_string_lossy().as_ref()],
        )?;
        Ok(BoundSession {
            harness: harness.to_string(),
            session_id: session_id.to_string(),
            cwd: cwd.to_path_buf(),
        })
    }

    pub fn bound_session(&self) -> Result<Option<BoundSession>> {
        let harness = self.get_kv("harness")?;
        let session_id = self.get_kv("session_id")?;
        let cwd = self.get_kv("cwd")?;
        match (harness, session_id, cwd) {
            (Some(harness), Some(session_id), Some(cwd)) => Ok(Some(BoundSession {
                harness,
                session_id,
                cwd: PathBuf::from(cwd),
            })),
            _ => Ok(None),
        }
    }

    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("journal mutex");
        Ok(conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().expect("journal mutex");
        conn.execute(
            "INSERT INTO kv(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

pub fn history_to_entry(item: &HistoryItem) -> AgentTranscriptEntry {
    match item.kind.as_str() {
        "userMessage" => AgentTranscriptEntry {
            id: item.id.clone(),
            kind: "message".into(),
            role: Some("user".into()),
            content: serde_json::json!(item.text),
            created_at: item.created_at.clone(),
        },
        "agentMessage" => AgentTranscriptEntry {
            id: item.id.clone(),
            kind: "message".into(),
            role: Some("assistant".into()),
            content: serde_json::json!(item.text),
            created_at: item.created_at.clone(),
        },
        other => AgentTranscriptEntry {
            id: item.id.clone(),
            kind: other.to_string(),
            role: None,
            content: serde_json::json!({
                "text": item.text,
                "status": item.status,
                "preview_text": item.preview_text,
                "detail_text": item.detail_text,
            }),
            created_at: item.created_at.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn survives_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        {
            let journal = Journal::open(&path).unwrap();
            journal
                .upsert_entry(&AgentTranscriptEntry {
                    id: "u1".into(),
                    kind: "message".into(),
                    role: Some("user".into()),
                    content: serde_json::json!("hello"),
                    created_at: Some("2026-09-04T00:00:00.000Z".into()),
                })
                .unwrap();
        }
        let journal = Journal::open(&path).unwrap();
        let entries = journal.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "u1");
        assert_eq!(entries[0].content, serde_json::json!("hello"));
    }

    #[test]
    fn operation_ids_are_idempotent() {
        let dir = tempdir().unwrap();
        let journal = Journal::open(&dir.path().join("journal.sqlite")).unwrap();
        assert!(!journal.claim_operation("op-1").unwrap());
        assert!(journal.claim_operation("op-1").unwrap());
    }
}
