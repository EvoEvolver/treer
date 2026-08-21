use std::collections::HashSet;

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use sqlx::any::AnyPoolOptions;
use sqlx::{Any, AnyPool, Row};
use treer_protocol::{AppPrincipal, AppPrincipalKind};
use uuid::Uuid;

use crate::model::{Delivery, HumanSession, MailboxResponse, Message, PendingOAuth};

const MAX_BODY_BYTES: usize = 32 * 1024;
const MAX_RECIPIENTS: usize = 32;
const MAX_CONTEXTS: usize = 32;
const MAX_INBOX_LIMIT: u16 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabaseKind {
    Sqlite,
    Postgres,
}

#[derive(Clone)]
pub struct MailStore {
    pool: AnyPool,
    kind: DatabaseKind,
}

impl MailStore {
    pub async fn open(database_url: &str) -> anyhow::Result<Self> {
        sqlx::any::install_default_drivers();
        let kind = if database_url.starts_with("sqlite:") {
            DatabaseKind::Sqlite
        } else if database_url.starts_with("postgres:") || database_url.starts_with("postgresql:") {
            DatabaseKind::Postgres
        } else {
            bail!("mail DATABASE_URL must use sqlite, postgres, or postgresql");
        };
        let max_connections = if kind == DatabaseKind::Sqlite { 1 } else { 10 };
        let pool = AnyPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await
            .context("connect to mail database")?;
        if kind == DatabaseKind::Sqlite {
            sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&pool)
                .await
                .context("enable SQLite foreign keys")?;
            sqlx::query("PRAGMA busy_timeout = 5000")
                .execute(&pool)
                .await
                .context("configure SQLite busy timeout")?;
        }
        let store = Self { pool, kind };
        store.initialize().await?;
        Ok(store)
    }

    async fn initialize(&self) -> anyhow::Result<()> {
        for statement in SCHEMA {
            sqlx::query(statement)
                .execute(&self.pool)
                .await
                .with_context(|| format!("initialize mail schema: {statement}"))?;
        }
        Ok(())
    }

    pub async fn save_oauth_state(&self, state: &PendingOAuth) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM oauth_states WHERE expires_at <= $1")
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO oauth_states(state_hash, verifier, return_path, expires_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&state.state_hash)
        .bind(&state.verifier)
        .bind(&state.return_path)
        .bind(state.expires_at.to_rfc3339())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn consume_oauth_state(
        &self,
        state_hash: &str,
    ) -> anyhow::Result<Option<PendingOAuth>> {
        let row = sqlx::query(
            "DELETE FROM oauth_states WHERE state_hash = $1 AND expires_at > $2 \
             RETURNING state_hash, verifier, return_path, expires_at",
        )
        .bind(state_hash)
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(PendingOAuth {
                state_hash: row.get("state_hash"),
                verifier: row.get("verifier"),
                return_path: row.get("return_path"),
                expires_at: parse_timestamp(row.get("expires_at"))?,
            })
        })
        .transpose()
    }

    pub async fn save_session(&self, session: &HumanSession) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM human_sessions WHERE expires_at <= $1")
            .bind(Utc::now().to_rfc3339())
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO human_sessions(token_hash, access_token, workspace_id, service_id, \
             user_id, preferred_name, role, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&session.token_hash)
        .bind(&session.access_token)
        .bind(&session.workspace_id)
        .bind(&session.service_id)
        .bind(&session.user_id)
        .bind(&session.preferred_name)
        .bind(&session.role)
        .bind(session.expires_at.to_rfc3339())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn session(&self, token_hash: &str) -> anyhow::Result<Option<HumanSession>> {
        let row = sqlx::query(
            "SELECT token_hash, access_token, workspace_id, service_id, user_id, preferred_name, \
             role, expires_at FROM human_sessions WHERE token_hash = $1 AND expires_at > $2",
        )
        .bind(token_hash)
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await?;
        row.map(human_session_from_row).transpose()
    }

    pub async fn delete_session(&self, token_hash: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM human_sessions WHERE token_hash = $1")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn send_message(
        &self,
        workspace_id: &str,
        sender: &AppPrincipal,
        recipients: &[AppPrincipal],
        context_ids: &[String],
        body: &str,
    ) -> anyhow::Result<Message> {
        if body.trim().is_empty() || body.len() > MAX_BODY_BYTES {
            bail!("message body must contain 1-{MAX_BODY_BYTES} bytes");
        }
        if recipients.is_empty() || recipients.len() > MAX_RECIPIENTS {
            bail!("message must have 1-{MAX_RECIPIENTS} recipients");
        }
        if context_ids.len() > MAX_CONTEXTS {
            bail!("message may reference at most {MAX_CONTEXTS} context messages");
        }
        let recipient_keys = recipients
            .iter()
            .map(|recipient| (principal_kind(recipient.kind), recipient.id.as_str()))
            .collect::<HashSet<_>>();
        if recipient_keys.len() != recipients.len() {
            bail!("message recipients must be unique");
        }
        let context_keys = context_ids.iter().collect::<HashSet<_>>();
        if context_keys.len() != context_ids.len() {
            bail!("message context IDs must be unique");
        }

        let message_id = format!("msg_{}", Uuid::new_v4().simple());
        let created_at = Utc::now();
        let mut transaction = self.pool.begin().await?;
        for context_id in context_ids {
            let count: i64 = sqlx::query(
                "SELECT COUNT(*) AS count FROM messages m WHERE m.message_id = $1 \
                 AND m.workspace_id = $2 AND ((m.sender_kind = $3 AND m.sender_id = $4) \
                 OR EXISTS (SELECT 1 FROM recipients r WHERE r.message_id = m.message_id \
                 AND r.recipient_kind = $3 AND r.recipient_id = $4))",
            )
            .bind(context_id)
            .bind(workspace_id)
            .bind(principal_kind(sender.kind))
            .bind(&sender.id)
            .fetch_one(&mut *transaction)
            .await?
            .get("count");
            if count == 0 {
                bail!("context message {context_id} is not visible to the sender");
            }
        }

        sqlx::query(
            "INSERT INTO messages(message_id, workspace_id, sender_kind, sender_id, sender_name, \
             sender_role, body, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&message_id)
        .bind(workspace_id)
        .bind(principal_kind(sender.kind))
        .bind(&sender.id)
        .bind(&sender.name)
        .bind(sender.role.as_deref())
        .bind(body)
        .bind(created_at.to_rfc3339())
        .execute(&mut *transaction)
        .await?;

        for (position, recipient) in recipients.iter().enumerate() {
            sqlx::query(
                "INSERT INTO recipients(message_id, workspace_id, recipient_kind, recipient_id, \
                 recipient_name, recipient_role, position, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(&message_id)
            .bind(workspace_id)
            .bind(principal_kind(recipient.kind))
            .bind(&recipient.id)
            .bind(&recipient.name)
            .bind(recipient.role.as_deref())
            .bind(position as i64)
            .bind(created_at.to_rfc3339())
            .execute(&mut *transaction)
            .await?;
        }
        for (position, context_id) in context_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO contexts(message_id, context_message_id, position) \
                 VALUES ($1, $2, $3)",
            )
            .bind(&message_id)
            .bind(context_id)
            .bind(position as i64)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(Message {
            message_id,
            workspace_id: workspace_id.to_string(),
            sender: sender.clone(),
            recipients: recipients.to_vec(),
            context_ids: context_ids.to_vec(),
            body: body.to_string(),
            created_at,
        })
    }

    pub async fn unread_inbox(
        &self,
        workspace_id: &str,
        recipient: &AppPrincipal,
        limit: u16,
    ) -> anyhow::Result<MailboxResponse> {
        validate_limit(limit)?;
        let now = Utc::now().to_rfc3339();
        let kind = principal_kind(recipient.kind);
        let mut transaction = self.pool.begin().await?;
        let claimed = match self.kind {
            DatabaseKind::Postgres => {
                sqlx::query(
                    "WITH picked AS (SELECT message_id FROM recipients WHERE workspace_id = $1 \
                 AND recipient_kind = $2 AND recipient_id = $3 AND read_at IS NULL \
                 ORDER BY created_at, message_id LIMIT $4 FOR UPDATE SKIP LOCKED) \
                 UPDATE recipients r SET read_at = $5 FROM picked p \
                 WHERE r.message_id = p.message_id AND r.recipient_kind = $2 \
                 AND r.recipient_id = $3 RETURNING r.message_id",
                )
                .bind(workspace_id)
                .bind(kind)
                .bind(&recipient.id)
                .bind(i64::from(limit))
                .bind(&now)
                .fetch_all(&mut *transaction)
                .await?
            }
            DatabaseKind::Sqlite => sqlx::query(
                "UPDATE recipients SET read_at = $1 WHERE rowid IN (SELECT rowid FROM recipients \
                 WHERE workspace_id = $2 AND recipient_kind = $3 AND recipient_id = $4 \
                 AND read_at IS NULL ORDER BY created_at, message_id LIMIT $5) \
                 RETURNING message_id",
            )
            .bind(&now)
            .bind(workspace_id)
            .bind(kind)
            .bind(&recipient.id)
            .bind(i64::from(limit))
            .fetch_all(&mut *transaction)
            .await?,
        };
        let ids = claimed
            .into_iter()
            .map(|row| row.get::<String, _>("message_id"))
            .collect::<Vec<_>>();
        let messages = load_messages(&mut transaction, &ids).await?;
        let remaining_unread =
            unread_count(&mut transaction, workspace_id, kind, &recipient.id).await?;
        transaction.commit().await?;
        Ok(MailboxResponse {
            deliveries: messages
                .into_iter()
                .map(|message| Delivery {
                    message,
                    unread: true,
                })
                .collect(),
            remaining_unread,
        })
    }

    pub async fn recent_mailbox(
        &self,
        workspace_id: &str,
        recipient: &AppPrincipal,
        limit: u16,
    ) -> anyhow::Result<MailboxResponse> {
        validate_limit(limit)?;
        let kind = principal_kind(recipient.kind);
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT message_id, read_at FROM recipients WHERE workspace_id = $1 \
             AND recipient_kind = $2 AND recipient_id = $3 \
             ORDER BY created_at DESC, message_id DESC LIMIT $4",
        )
        .bind(workspace_id)
        .bind(kind)
        .bind(&recipient.id)
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await?;
        let ids = rows
            .iter()
            .map(|row| row.get::<String, _>("message_id"))
            .collect::<Vec<_>>();
        let unread_ids = rows
            .iter()
            .filter(|row| row.get::<Option<String>, _>("read_at").is_none())
            .map(|row| row.get::<String, _>("message_id"))
            .collect::<HashSet<_>>();
        if !unread_ids.is_empty() {
            for id in &unread_ids {
                sqlx::query(
                    "UPDATE recipients SET read_at = $1 WHERE workspace_id = $2 \
                     AND recipient_kind = $3 AND recipient_id = $4 \
                     AND message_id = $5 AND read_at IS NULL",
                )
                .bind(Utc::now().to_rfc3339())
                .bind(workspace_id)
                .bind(kind)
                .bind(&recipient.id)
                .bind(id)
                .execute(&mut *transaction)
                .await?;
            }
        }
        let messages = load_messages(&mut transaction, &ids).await?;
        let remaining_unread =
            unread_count(&mut transaction, workspace_id, kind, &recipient.id).await?;
        transaction.commit().await?;
        Ok(MailboxResponse {
            deliveries: messages
                .into_iter()
                .map(|message| Delivery {
                    unread: unread_ids.contains(&message.message_id),
                    message,
                })
                .collect(),
            remaining_unread,
        })
    }
}

async fn unread_count(
    transaction: &mut sqlx::Transaction<'_, Any>,
    workspace_id: &str,
    kind: &str,
    recipient_id: &str,
) -> anyhow::Result<u64> {
    let count: i64 = sqlx::query(
        "SELECT COUNT(*) AS count FROM recipients WHERE workspace_id = $1 \
         AND recipient_kind = $2 AND recipient_id = $3 AND read_at IS NULL",
    )
    .bind(workspace_id)
    .bind(kind)
    .bind(recipient_id)
    .fetch_one(&mut **transaction)
    .await?
    .get("count");
    Ok(u64::try_from(count).unwrap_or(0))
}

async fn load_messages(
    transaction: &mut sqlx::Transaction<'_, Any>,
    ids: &[String],
) -> anyhow::Result<Vec<Message>> {
    let mut messages = Vec::with_capacity(ids.len());
    for id in ids {
        let row = sqlx::query(
            "SELECT message_id, workspace_id, sender_kind, sender_id, sender_name, sender_role, \
             body, created_at FROM messages WHERE message_id = $1",
        )
        .bind(id)
        .fetch_one(&mut **transaction)
        .await?;
        let recipient_rows = sqlx::query(
            "SELECT recipient_kind, recipient_id, recipient_name, recipient_role FROM recipients \
             WHERE message_id = $1 ORDER BY position",
        )
        .bind(id)
        .fetch_all(&mut **transaction)
        .await?;
        let context_rows = sqlx::query(
            "SELECT context_message_id FROM contexts WHERE message_id = $1 ORDER BY position",
        )
        .bind(id)
        .fetch_all(&mut **transaction)
        .await?;
        messages.push(Message {
            message_id: row.get("message_id"),
            workspace_id: row.get("workspace_id"),
            sender: AppPrincipal {
                kind: parse_principal_kind(row.get("sender_kind"))?,
                id: row.get("sender_id"),
                name: row.get("sender_name"),
                role: row.get("sender_role"),
            },
            recipients: recipient_rows
                .into_iter()
                .map(|row| {
                    Ok(AppPrincipal {
                        kind: parse_principal_kind(row.get("recipient_kind"))?,
                        id: row.get("recipient_id"),
                        name: row.get("recipient_name"),
                        role: row.get("recipient_role"),
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
            context_ids: context_rows
                .into_iter()
                .map(|row| row.get("context_message_id"))
                .collect(),
            body: row.get("body"),
            created_at: parse_timestamp(row.get("created_at"))?,
        });
    }
    Ok(messages)
}

fn human_session_from_row(row: sqlx::any::AnyRow) -> anyhow::Result<HumanSession> {
    Ok(HumanSession {
        token_hash: row.get("token_hash"),
        access_token: row.get("access_token"),
        workspace_id: row.get("workspace_id"),
        service_id: row.get("service_id"),
        user_id: row.get("user_id"),
        preferred_name: row.get("preferred_name"),
        role: row.get("role"),
        expires_at: parse_timestamp(row.get("expires_at"))?,
    })
}

fn principal_kind(kind: AppPrincipalKind) -> &'static str {
    match kind {
        AppPrincipalKind::Agent => "agent",
        AppPrincipalKind::Human => "human",
    }
}

fn parse_principal_kind(value: String) -> anyhow::Result<AppPrincipalKind> {
    match value.as_str() {
        "agent" => Ok(AppPrincipalKind::Agent),
        "human" => Ok(AppPrincipalKind::Human),
        _ => bail!("stored principal kind is invalid"),
    }
}

fn parse_timestamp(value: String) -> anyhow::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .context("stored timestamp is invalid")
}

fn validate_limit(limit: u16) -> anyhow::Result<()> {
    if limit == 0 || limit > MAX_INBOX_LIMIT {
        bail!("inbox limit must be between 1 and {MAX_INBOX_LIMIT}");
    }
    Ok(())
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS messages (\
        message_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, sender_kind TEXT NOT NULL, \
        sender_id TEXT NOT NULL, sender_name TEXT NOT NULL, sender_role TEXT, body TEXT NOT NULL, \
        created_at TEXT NOT NULL, CHECK(sender_kind IN ('agent', 'human')))",
    "CREATE INDEX IF NOT EXISTS messages_workspace_created \
        ON messages(workspace_id, created_at, message_id)",
    "CREATE TABLE IF NOT EXISTS recipients (\
        message_id TEXT NOT NULL, workspace_id TEXT NOT NULL, recipient_kind TEXT NOT NULL, \
        recipient_id TEXT NOT NULL, recipient_name TEXT NOT NULL, recipient_role TEXT, \
        position BIGINT NOT NULL, created_at TEXT NOT NULL, read_at TEXT, \
        PRIMARY KEY(message_id, recipient_kind, recipient_id), \
        FOREIGN KEY(message_id) REFERENCES messages(message_id) ON DELETE CASCADE, \
        CHECK(recipient_kind IN ('agent', 'human')))",
    "CREATE INDEX IF NOT EXISTS recipients_unread \
        ON recipients(workspace_id, recipient_kind, recipient_id, created_at, message_id)",
    "CREATE TABLE IF NOT EXISTS contexts (\
        message_id TEXT NOT NULL, context_message_id TEXT NOT NULL, position BIGINT NOT NULL, \
        PRIMARY KEY(message_id, context_message_id), \
        FOREIGN KEY(message_id) REFERENCES messages(message_id) ON DELETE CASCADE, \
        FOREIGN KEY(context_message_id) REFERENCES messages(message_id) ON DELETE CASCADE)",
    "CREATE TABLE IF NOT EXISTS oauth_states (\
        state_hash TEXT PRIMARY KEY, verifier TEXT NOT NULL, return_path TEXT NOT NULL, \
        expires_at TEXT NOT NULL)",
    "CREATE INDEX IF NOT EXISTS oauth_states_expiry ON oauth_states(expires_at)",
    "CREATE TABLE IF NOT EXISTS human_sessions (\
        token_hash TEXT PRIMARY KEY, access_token TEXT NOT NULL, workspace_id TEXT NOT NULL, \
        service_id TEXT NOT NULL, user_id TEXT NOT NULL, preferred_name TEXT NOT NULL, \
        role TEXT NOT NULL, expires_at TEXT NOT NULL)",
    "CREATE INDEX IF NOT EXISTS human_sessions_expiry ON human_sessions(expires_at)",
];

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    fn principal(kind: AppPrincipalKind, id: &str, name: &str) -> AppPrincipal {
        AppPrincipal {
            kind,
            id: id.to_string(),
            name: name.to_string(),
            role: (kind == AppPrincipalKind::Human).then(|| "member".to_string()),
        }
    }

    #[tokio::test]
    async fn sqlite_message_delivery_is_transactional_and_pull_only() {
        let store = MailStore::open("sqlite::memory:")
            .await
            .expect("open SQLite mail store");
        exercise_store(&store).await;
    }

    #[tokio::test]
    async fn postgres_message_delivery_is_transactional_and_pull_only() {
        let admin_url = std::env::var("TREER_MAIL_TEST_POSTGRES_URL")
            .unwrap_or_else(|_| "postgres://treer:treer@127.0.0.1:55432/treer_test".to_string());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("connect to PostgreSQL test container");
        let database = format!("treer_mail_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&admin)
            .await
            .expect("create isolated mail test database");
        let mut url = url::Url::parse(&admin_url).expect("parse PostgreSQL test URL");
        url.set_path(&format!("/{database}"));
        let store = MailStore::open(url.as_str())
            .await
            .expect("open PostgreSQL mail store");
        exercise_store(&store).await;
        store.pool.close().await;
        sqlx::query(&format!("DROP DATABASE {database}"))
            .execute(&admin)
            .await
            .expect("drop isolated mail test database");
    }

    async fn exercise_store(store: &MailStore) {
        let sender = principal(AppPrincipalKind::Agent, "agent-a", "builder");
        let recipient = principal(AppPrincipalKind::Human, "user-a", "Owner");
        let reviewer = principal(AppPrincipalKind::Agent, "agent-b", "reviewer");
        let first = store
            .send_message(
                "workspace-a",
                &sender,
                &[recipient.clone(), reviewer.clone()],
                &[],
                "Ready",
            )
            .await
            .expect("send first message");
        store
            .send_message(
                "workspace-a",
                &sender,
                std::slice::from_ref(&recipient),
                std::slice::from_ref(&first.message_id),
                "Follow-up",
            )
            .await
            .expect("send contextual message");
        let branch = store
            .send_message(
                "workspace-a",
                &reviewer,
                std::slice::from_ref(&recipient),
                std::slice::from_ref(&first.message_id),
                "Independent review",
            )
            .await
            .expect("send branch");
        let merge = store
            .send_message(
                "workspace-a",
                &recipient,
                std::slice::from_ref(&sender),
                &[first.message_id.clone(), branch.message_id.clone()],
                "Merged response",
            )
            .await
            .expect("send multi-parent response");
        assert_eq!(
            merge.context_ids,
            [first.message_id.clone(), branch.message_id]
        );
        let invisible = store
            .send_message(
                "workspace-b",
                &recipient,
                std::slice::from_ref(&sender),
                std::slice::from_ref(&first.message_id),
                "Cross-workspace context",
            )
            .await
            .expect_err("cross-workspace context must fail");
        assert!(invisible.to_string().contains("not visible"));
        let inbox = store
            .unread_inbox("workspace-a", &recipient, 1)
            .await
            .expect("read bounded inbox");
        assert_eq!(inbox.deliveries.len(), 1);
        assert_eq!(inbox.remaining_unread, 2);
        let history = store
            .recent_mailbox("workspace-a", &recipient, 100)
            .await
            .expect("read recent history");
        assert_eq!(history.deliveries.len(), 3);
        assert_eq!(history.remaining_unread, 0);
        assert_eq!(history.deliveries[0].message.body, "Independent review");
        assert_eq!(
            history.deliveries[1].message.context_ids,
            [first.message_id]
        );
    }

    #[test]
    fn migration_fixtures_capture_branched_graph_and_session_states() {
        for fixture in [
            include_str!("../tests/fixtures/legacy-mail-v1.sqlite.sql"),
            include_str!("../tests/fixtures/legacy-mail-v1.postgres.sql"),
        ] {
            assert!(fixture.contains("legacy_branch_a"));
            assert!(fixture.contains("legacy_branch_b"));
            assert!(fixture.contains("('legacy_merge', 'legacy_branch_a', 0)"));
            assert!(fixture.contains("('legacy_merge', 'legacy_branch_b', 1)"));
            assert!(fixture.contains("active-session"));
            assert!(fixture.contains("expired-session"));
            assert!(fixture.contains("'agent'"));
            assert!(fixture.contains("'human'"));
            assert!(fixture.contains("NULL)"));
        }
    }
}
