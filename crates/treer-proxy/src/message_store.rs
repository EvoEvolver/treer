use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{PgConnection, Row};
use tokio::sync::Notify;
use treer_protocol::{
    AcknowledgeMessagesRequest, AcknowledgeMessagesResponse, CoreMessage, DomainEventActor,
    DomainEventEnvelope, DomainEventResource, ImportMessagesRequest, ImportMessagesResponse,
    LegacyMailMessage, MessageDelivery, MessageExternalSource, MessagePage, MessagePrincipal,
    MessagePrincipalKind, ReceiveMessagesResponse, SendMessageRequest, SendMessageResponse,
    DOMAIN_EVENT_SCHEMA_VERSION, MAX_MESSAGE_BODY_BYTES, MAX_MESSAGE_CONTEXTS,
    MAX_MESSAGE_EXTERNAL_METADATA_ENTRIES, MAX_MESSAGE_IDEMPOTENCY_KEY_BYTES,
    MAX_MESSAGE_PAGE_SIZE, MAX_MESSAGE_RECIPIENTS, MAX_MESSAGE_WAIT_MILLISECONDS,
    MESSAGE_SCHEMA_VERSION,
};
use uuid::Uuid;

use crate::event_bus::EventBus;

const MAX_IMPORT_MESSAGES: usize = 1_000;
const MAX_ACK_DELIVERIES: usize = 100;
const OUTBOX_BATCH_SIZE: i64 = 100;

#[derive(Clone)]
pub struct MessageStore {
    pool: PgPool,
    changed: Arc<Notify>,
}

#[derive(Debug, thiserror::Error)]
pub enum MessageStoreError {
    #[error("{message}")]
    Contract { code: &'static str, message: String },
    #[error("message storage operation failed")]
    Database(#[source] sqlx::Error),
    #[error("stored message data is invalid")]
    Corrupt,
}

impl MessageStoreError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Contract { code, .. } => code,
            Self::Database(_) => "message_store_unavailable",
            Self::Corrupt => "message_store_corrupt",
        }
    }

    fn contract(code: &'static str, message: impl Into<String>) -> Self {
        Self::Contract {
            code,
            message: message.into(),
        }
    }
}

impl From<sqlx::Error> for MessageStoreError {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value)
    }
}

impl MessageStore {
    pub async fn open(pool: PgPool) -> Result<Self, MessageStoreError> {
        let store = Self {
            pool,
            changed: Arc::new(Notify::new()),
        };
        store.initialize().await?;
        Ok(store)
    }

    async fn initialize(&self) -> Result<(), MessageStoreError> {
        for statement in SCHEMA {
            sqlx::query(statement).execute(&self.pool).await?;
        }
        Ok(())
    }

    pub fn spawn_outbox_dispatcher(&self, event_bus: EventBus) {
        let store = self.clone();
        tokio::spawn(async move {
            loop {
                match store.dispatch_pending(&event_bus).await {
                    Ok(0) => {
                        tokio::select! {
                            () = store.changed.notified() => {}
                            () = tokio::time::sleep(Duration::from_secs(2)) => {}
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(code = error.code(), "message outbox dispatch failed");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        });
    }

    pub async fn send(
        &self,
        workspace_id: &str,
        sender: &MessagePrincipal,
        recipients: &[MessagePrincipal],
        request: &SendMessageRequest,
    ) -> Result<SendMessageResponse, MessageStoreError> {
        validate_workspace_and_principal(workspace_id, sender)?;
        validate_send(recipients, request)?;
        let request_hash = request_hash(&SendFingerprint {
            recipients,
            context_ids: &request.context_ids,
            body: &request.body,
            expires_at: request.expires_at.as_ref(),
            correlation_id: request.correlation_id.as_deref(),
            trace_id: request.trace_id.as_deref(),
            external_source: request.external_source.as_ref(),
        })?;
        let mut transaction = self.pool.begin().await?;
        let connection = &mut *transaction;

        if let Some(key) = request.idempotency_key.as_deref() {
            lock_idempotency(connection, workspace_id, sender, key).await?;
            if let Some(row) = sqlx::query(
                "SELECT request_hash, message_id FROM core_message_idempotency \
                 WHERE workspace_id = $1 AND sender_kind = $2 AND sender_id = $3 \
                 AND idempotency_key = $4",
            )
            .bind(workspace_id)
            .bind(sender.kind.as_str())
            .bind(&sender.id)
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?
            {
                let stored_hash: String = row.get("request_hash");
                if stored_hash != request_hash {
                    return Err(MessageStoreError::contract(
                        "message_idempotency_conflict",
                        "idempotency key was already used for a different message",
                    ));
                }
                let message_id: String = row.get("message_id");
                let message = load_message(&mut *connection, workspace_id, &message_id).await?;
                let delivery_ids =
                    delivery_ids(&mut *connection, workspace_id, &message_id).await?;
                transaction.commit().await?;
                return Ok(SendMessageResponse {
                    message,
                    delivery_ids,
                    idempotent_replay: true,
                });
            }
        }

        for context_id in &request.context_ids {
            let visible: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM core_messages m WHERE m.workspace_id = $1 \
                 AND m.message_id = $2 AND (\
                    (m.sender_kind = $3 AND m.sender_id = $4) OR EXISTS(\
                        SELECT 1 FROM core_message_deliveries d \
                        WHERE d.workspace_id = m.workspace_id AND d.message_id = m.message_id \
                        AND d.recipient_kind = $3 AND d.recipient_id = $4)))",
            )
            .bind(workspace_id)
            .bind(context_id)
            .bind(sender.kind.as_str())
            .bind(&sender.id)
            .fetch_one(&mut *connection)
            .await?;
            if !visible {
                return Err(MessageStoreError::contract(
                    "message_context_not_found",
                    "a context message does not exist or is not visible",
                ));
            }
        }

        let now = Utc::now();
        let message_id = format!("msg_{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO core_messages(\
                message_id, workspace_id, sender_kind, sender_id, sender_name, sender_role, body, \
                created_at, expires_at, correlation_id, trace_id, external_source) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(&message_id)
        .bind(workspace_id)
        .bind(sender.kind.as_str())
        .bind(&sender.id)
        .bind(&sender.name)
        .bind(&sender.role)
        .bind(&request.body)
        .bind(now.to_rfc3339())
        .bind(request.expires_at.map(|value| value.to_rfc3339()))
        .bind(&request.correlation_id)
        .bind(&request.trace_id)
        .bind(
            request
                .external_source
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| MessageStoreError::Corrupt)?,
        )
        .execute(&mut *connection)
        .await?;

        let mut delivery_ids = Vec::with_capacity(recipients.len());
        for (position, recipient) in recipients.iter().enumerate() {
            let delivery_id = format!("dlv_{}", Uuid::new_v4().simple());
            sqlx::query(
                "INSERT INTO core_message_deliveries(\
                    delivery_id, message_id, workspace_id, recipient_kind, recipient_id, \
                    recipient_name, recipient_role, position, created_at, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(&delivery_id)
            .bind(&message_id)
            .bind(workspace_id)
            .bind(recipient.kind.as_str())
            .bind(&recipient.id)
            .bind(&recipient.name)
            .bind(&recipient.role)
            .bind(position as i64)
            .bind(now.to_rfc3339())
            .bind(request.expires_at.map(|value| value.to_rfc3339()))
            .execute(&mut *connection)
            .await?;
            delivery_ids.push(delivery_id);
        }
        for (position, context_id) in request.context_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO core_message_contexts(\
                    message_id, workspace_id, context_message_id, position) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(&message_id)
            .bind(workspace_id)
            .bind(context_id)
            .bind(position as i64)
            .execute(&mut *connection)
            .await?;
        }
        if let Some(key) = request.idempotency_key.as_deref() {
            sqlx::query(
                "INSERT INTO core_message_idempotency(\
                    workspace_id, sender_kind, sender_id, idempotency_key, request_hash, \
                    message_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(workspace_id)
            .bind(sender.kind.as_str())
            .bind(&sender.id)
            .bind(key)
            .bind(&request_hash)
            .bind(&message_id)
            .bind(now.to_rfc3339())
            .execute(&mut *connection)
            .await?;
        }
        insert_outbox(
            &mut *connection,
            DomainEventEnvelope {
                event_id: format!("evt_{}", Uuid::new_v4().simple()),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
                organization_id: None,
                workspace_id: workspace_id.to_string(),
                actor: DomainEventActor {
                    kind: sender.kind.as_str().to_string(),
                    id: Some(sender.id.clone()),
                },
                action: "message.created".to_string(),
                resource: DomainEventResource {
                    kind: "message".to_string(),
                    id: message_id.clone(),
                },
                occurred_at: now,
                trace_id: request.trace_id.clone(),
                causation_id: None,
                correlation_id: request.correlation_id.clone(),
                workspace_revision: None,
                payload: json!({
                    "message_id": message_id,
                    "delivery_ids": delivery_ids,
                    "context_ids": request.context_ids,
                    "recipient_ids": recipients.iter().map(|recipient| &recipient.id).collect::<Vec<_>>()
                }),
            },
        )
        .await?;
        let message = load_message(&mut *connection, workspace_id, &message_id).await?;
        transaction.commit().await?;
        self.changed.notify_waiters();
        Ok(SendMessageResponse {
            message,
            delivery_ids,
            idempotent_replay: false,
        })
    }

    pub async fn get(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        message_id: &str,
    ) -> Result<CoreMessage, MessageStoreError> {
        let mut connection = self.pool.acquire().await?;
        ensure_visible(&mut connection, workspace_id, principal, message_id).await?;
        load_message(&mut connection, workspace_id, message_id).await
    }

    pub async fn list(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        before: Option<&str>,
        limit: u16,
    ) -> Result<MessagePage, MessageStoreError> {
        validate_page_size(limit)?;
        let cursor = before.map(decode_cursor).transpose()?;
        let mut connection = self.pool.acquire().await?;
        let rows = if let Some((created_at, message_id)) = cursor {
            sqlx::query(
                "SELECT m.message_id, m.created_at FROM core_messages m \
                 WHERE m.workspace_id = $1 AND (\
                    (m.sender_kind = $2 AND m.sender_id = $3) OR EXISTS(\
                        SELECT 1 FROM core_message_deliveries d WHERE d.workspace_id = m.workspace_id \
                        AND d.message_id = m.message_id AND d.recipient_kind = $2 \
                        AND d.recipient_id = $3)) \
                 AND (m.created_at < $4 OR (m.created_at = $4 AND m.message_id < $5)) \
                 ORDER BY m.created_at DESC, m.message_id DESC LIMIT $6",
            )
            .bind(workspace_id)
            .bind(principal.kind.as_str())
            .bind(&principal.id)
            .bind(created_at.to_rfc3339())
            .bind(message_id)
            .bind(i64::from(limit) + 1)
            .fetch_all(&mut *connection)
            .await?
        } else {
            sqlx::query(
                "SELECT m.message_id, m.created_at FROM core_messages m \
                 WHERE m.workspace_id = $1 AND (\
                    (m.sender_kind = $2 AND m.sender_id = $3) OR EXISTS(\
                        SELECT 1 FROM core_message_deliveries d WHERE d.workspace_id = m.workspace_id \
                        AND d.message_id = m.message_id AND d.recipient_kind = $2 \
                        AND d.recipient_id = $3)) \
                 ORDER BY m.created_at DESC, m.message_id DESC LIMIT $4",
            )
            .bind(workspace_id)
            .bind(principal.kind.as_str())
            .bind(&principal.id)
            .bind(i64::from(limit) + 1)
            .fetch_all(&mut *connection)
            .await?
        };
        let has_more = rows.len() > usize::from(limit);
        let page_rows = rows
            .into_iter()
            .take(usize::from(limit))
            .collect::<Vec<_>>();
        let mut messages = Vec::with_capacity(page_rows.len());
        for row in &page_rows {
            messages.push(
                load_message(
                    &mut connection,
                    workspace_id,
                    &row.get::<String, _>("message_id"),
                )
                .await?,
            );
        }
        let next_cursor = if has_more {
            page_rows.last().map(|row| {
                encode_cursor(
                    &parse_timestamp(row.get::<String, _>("created_at"))
                        .expect("query only returns valid stored timestamps"),
                    &row.get::<String, _>("message_id"),
                )
            })
        } else {
            None
        };
        let remaining_unacknowledged =
            unacknowledged_count(&mut connection, workspace_id, principal).await?;
        Ok(MessagePage {
            messages,
            next_cursor,
            remaining_unacknowledged,
        })
    }

    pub async fn receive(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        limit: u16,
        wait_milliseconds: u64,
    ) -> Result<ReceiveMessagesResponse, MessageStoreError> {
        validate_page_size(limit)?;
        if wait_milliseconds > MAX_MESSAGE_WAIT_MILLISECONDS {
            return Err(MessageStoreError::contract(
                "message_wait_invalid",
                format!(
                    "message wait must be at most {MAX_MESSAGE_WAIT_MILLISECONDS} milliseconds"
                ),
            ));
        }
        let first = self.receive_now(workspace_id, principal, limit).await?;
        if !first.deliveries.is_empty() || wait_milliseconds == 0 {
            return Ok(first);
        }
        let notified = self.changed.notified();
        let _ = tokio::time::timeout(Duration::from_millis(wait_milliseconds), notified).await;
        self.receive_now(workspace_id, principal, limit).await
    }

    async fn receive_now(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        limit: u16,
    ) -> Result<ReceiveMessagesResponse, MessageStoreError> {
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query(
            "SELECT delivery_id, message_id, recipient_kind, recipient_id, recipient_name, \
                    recipient_role, created_at, acknowledged_at, expires_at \
             FROM core_message_deliveries WHERE workspace_id = $1 AND recipient_kind = $2 \
             AND recipient_id = $3 AND acknowledged_at IS NULL \
             AND (expires_at IS NULL OR expires_at > $4) \
             ORDER BY created_at, delivery_id LIMIT $5",
        )
        .bind(workspace_id)
        .bind(principal.kind.as_str())
        .bind(&principal.id)
        .bind(Utc::now().to_rfc3339())
        .bind(i64::from(limit))
        .fetch_all(&mut *connection)
        .await?;
        let mut deliveries = Vec::with_capacity(rows.len());
        for row in rows {
            let message_id: String = row.get("message_id");
            deliveries.push(delivery_from_row(
                &row,
                load_message(&mut connection, workspace_id, &message_id).await?,
            )?);
        }
        let remaining_unacknowledged =
            unacknowledged_count(&mut connection, workspace_id, principal).await?;
        Ok(ReceiveMessagesResponse {
            deliveries,
            remaining_unacknowledged,
        })
    }

    pub async fn acknowledge(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        request: &AcknowledgeMessagesRequest,
    ) -> Result<AcknowledgeMessagesResponse, MessageStoreError> {
        validate_ack(request)?;
        let request_hash = request_hash(&request.delivery_ids)?;
        let mut transaction = self.pool.begin().await?;
        let connection = &mut *transaction;
        lock_idempotency(
            &mut *connection,
            workspace_id,
            principal,
            &request.operation_id,
        )
        .await?;
        if let Some(row) = sqlx::query(
            "SELECT request_hash, response FROM core_message_ack_operations \
             WHERE workspace_id = $1 AND principal_kind = $2 AND principal_id = $3 \
             AND operation_id = $4",
        )
        .bind(workspace_id)
        .bind(principal.kind.as_str())
        .bind(&principal.id)
        .bind(&request.operation_id)
        .fetch_optional(&mut *connection)
        .await?
        {
            if row.get::<String, _>("request_hash") != request_hash {
                return Err(MessageStoreError::contract(
                    "message_ack_idempotency_conflict",
                    "acknowledgement operation ID was used for a different delivery set",
                ));
            }
            let response = serde_json::from_value(row.get::<Value, _>("response"))
                .map_err(|_| MessageStoreError::Corrupt)?;
            transaction.commit().await?;
            return Ok(response);
        }

        let now = Utc::now();
        let mut acknowledged = Vec::new();
        let mut already = Vec::new();
        for delivery_id in &request.delivery_ids {
            let row = sqlx::query(
                "SELECT message_id, acknowledged_at FROM core_message_deliveries \
                 WHERE workspace_id = $1 AND delivery_id = $2 AND recipient_kind = $3 \
                 AND recipient_id = $4 FOR UPDATE",
            )
            .bind(workspace_id)
            .bind(delivery_id)
            .bind(principal.kind.as_str())
            .bind(&principal.id)
            .fetch_optional(&mut *connection)
            .await?
            .ok_or_else(|| {
                MessageStoreError::contract(
                    "message_delivery_not_found",
                    "a delivery does not exist or is not owned by this principal",
                )
            })?;
            if row.get::<Option<String>, _>("acknowledged_at").is_some() {
                already.push(delivery_id.clone());
                continue;
            }
            sqlx::query(
                "UPDATE core_message_deliveries SET acknowledged_at = $1 \
                 WHERE workspace_id = $2 AND delivery_id = $3",
            )
            .bind(now.to_rfc3339())
            .bind(workspace_id)
            .bind(delivery_id)
            .execute(&mut *connection)
            .await?;
            acknowledged.push(delivery_id.clone());
            let message_id: String = row.get("message_id");
            insert_outbox(
                &mut *connection,
                DomainEventEnvelope {
                    event_id: format!("evt_{}", Uuid::new_v4().simple()),
                    schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
                    organization_id: None,
                    workspace_id: workspace_id.to_string(),
                    actor: DomainEventActor {
                        kind: principal.kind.as_str().to_string(),
                        id: Some(principal.id.clone()),
                    },
                    action: "message.acknowledged".to_string(),
                    resource: DomainEventResource {
                        kind: "message.delivery".to_string(),
                        id: delivery_id.clone(),
                    },
                    occurred_at: now,
                    trace_id: None,
                    causation_id: None,
                    correlation_id: Some(request.operation_id.clone()),
                    workspace_revision: None,
                    payload: json!({
                        "delivery_id": delivery_id,
                        "message_id": message_id
                    }),
                },
            )
            .await?;
        }
        let response = AcknowledgeMessagesResponse {
            acknowledged_delivery_ids: acknowledged,
            already_acknowledged_delivery_ids: already,
        };
        sqlx::query(
            "INSERT INTO core_message_ack_operations(\
                workspace_id, principal_kind, principal_id, operation_id, request_hash, \
                response, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(workspace_id)
        .bind(principal.kind.as_str())
        .bind(&principal.id)
        .bind(&request.operation_id)
        .bind(request_hash)
        .bind(serde_json::to_value(&response).map_err(|_| MessageStoreError::Corrupt)?)
        .bind(now.to_rfc3339())
        .execute(&mut *connection)
        .await?;
        transaction.commit().await?;
        self.changed.notify_waiters();
        Ok(response)
    }

    pub async fn import_legacy_mail(
        &self,
        workspace_id: &str,
        importer: &MessagePrincipal,
        request: &ImportMessagesRequest,
    ) -> Result<ImportMessagesResponse, MessageStoreError> {
        validate_import(workspace_id, request)?;
        let request_hash = request_hash(&request.messages)?;
        let mut transaction = self.pool.begin().await?;
        let connection = &mut *transaction;
        lock_idempotency(
            &mut *connection,
            workspace_id,
            importer,
            &request.operation_id,
        )
        .await?;
        if let Some(row) = sqlx::query(
            "SELECT request_hash, response FROM core_message_import_operations \
             WHERE workspace_id = $1 AND operation_id = $2",
        )
        .bind(workspace_id)
        .bind(&request.operation_id)
        .fetch_optional(&mut *connection)
        .await?
        {
            if row.get::<String, _>("request_hash") != request_hash {
                return Err(MessageStoreError::contract(
                    "message_import_idempotency_conflict",
                    "import operation ID was used for different data",
                ));
            }
            let response = serde_json::from_value(row.get::<Value, _>("response"))
                .map_err(|_| MessageStoreError::Corrupt)?;
            transaction.commit().await?;
            return Ok(response);
        }

        let mut imported = 0_u64;
        let mut existing = 0_u64;
        let mut seen = HashSet::new();
        let mut message_ids = Vec::with_capacity(request.messages.len());
        for message in &request.messages {
            validate_legacy_message(workspace_id, message)?;
            if !seen.insert(message.message_id.clone()) {
                return Err(MessageStoreError::contract(
                    "message_import_duplicate",
                    "import batch contains a duplicate message ID",
                ));
            }
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM core_messages WHERE workspace_id = $1 AND message_id = $2)",
            )
            .bind(workspace_id)
            .bind(&message.message_id)
            .fetch_one(&mut *connection)
            .await?;
            if exists {
                if !legacy_message_matches(&mut *connection, message).await? {
                    return Err(MessageStoreError::contract(
                        "message_import_conflict",
                        "an imported Message ID already exists with different data",
                    ));
                }
                existing += 1;
                message_ids.push(message.message_id.clone());
                continue;
            }
            for context_id in &message.context_ids {
                let context_exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM core_messages WHERE workspace_id = $1 AND message_id = $2)",
                )
                .bind(workspace_id)
                .bind(context_id)
                .fetch_one(&mut *connection)
                .await?;
                if !context_exists {
                    return Err(MessageStoreError::contract(
                        "message_import_not_topological",
                        "legacy messages must be imported in topological order",
                    ));
                }
            }
            insert_legacy_message(&mut *connection, message).await?;
            imported += 1;
            message_ids.push(message.message_id.clone());
        }
        let response = ImportMessagesResponse {
            imported,
            existing,
            message_ids,
        };
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO core_message_import_operations(\
                workspace_id, operation_id, request_hash, response, created_at, actor_kind, actor_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(workspace_id)
        .bind(&request.operation_id)
        .bind(request_hash)
        .bind(serde_json::to_value(&response).map_err(|_| MessageStoreError::Corrupt)?)
        .bind(now.to_rfc3339())
        .bind(importer.kind.as_str())
        .bind(&importer.id)
        .execute(&mut *connection)
        .await?;
        insert_outbox(
            &mut *connection,
            DomainEventEnvelope {
                event_id: format!("evt_{}", Uuid::new_v4().simple()),
                schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
                organization_id: None,
                workspace_id: workspace_id.to_string(),
                actor: DomainEventActor {
                    kind: importer.kind.as_str().to_string(),
                    id: Some(importer.id.clone()),
                },
                action: "message.imported".to_string(),
                resource: DomainEventResource {
                    kind: "message.import".to_string(),
                    id: request.operation_id.clone(),
                },
                occurred_at: now,
                trace_id: None,
                causation_id: None,
                correlation_id: Some(request.operation_id.clone()),
                workspace_revision: None,
                payload: json!({"imported": imported, "existing": existing}),
            },
        )
        .await?;
        transaction.commit().await?;
        self.changed.notify_waiters();
        Ok(response)
    }

    async fn dispatch_pending(&self, event_bus: &EventBus) -> Result<usize, MessageStoreError> {
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT event_id, envelope FROM core_message_outbox WHERE dispatched_at IS NULL \
             ORDER BY created_at, event_id FOR UPDATE SKIP LOCKED LIMIT $1",
        )
        .bind(OUTBOX_BATCH_SIZE)
        .fetch_all(&mut *transaction)
        .await?;
        let mut dispatched = 0;
        for row in rows {
            let event_id: String = row.get("event_id");
            let envelope = serde_json::from_value::<DomainEventEnvelope>(row.get("envelope"))
                .map_err(|_| MessageStoreError::Corrupt)?;
            if event_bus.publish(envelope).is_err() {
                break;
            }
            sqlx::query(
                "UPDATE core_message_outbox SET dispatched_at = $1, attempts = attempts + 1 \
                 WHERE event_id = $2",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(event_id)
            .execute(&mut *transaction)
            .await?;
            dispatched += 1;
        }
        transaction.commit().await?;
        Ok(dispatched)
    }
}

#[derive(Serialize)]
struct SendFingerprint<'a> {
    recipients: &'a [MessagePrincipal],
    context_ids: &'a [String],
    body: &'a str,
    expires_at: Option<&'a DateTime<Utc>>,
    correlation_id: Option<&'a str>,
    trace_id: Option<&'a str>,
    external_source: Option<&'a MessageExternalSource>,
}

fn validate_workspace_and_principal(
    workspace_id: &str,
    principal: &MessagePrincipal,
) -> Result<(), MessageStoreError> {
    if workspace_id.is_empty() || workspace_id.len() > 256 || principal.id.is_empty() {
        return Err(MessageStoreError::contract(
            "message_principal_invalid",
            "workspace and principal IDs must be non-empty and bounded",
        ));
    }
    if principal.id.len() > 256 || principal.name.is_empty() || principal.name.len() > 256 {
        return Err(MessageStoreError::contract(
            "message_principal_invalid",
            "principal identity snapshot is invalid",
        ));
    }
    Ok(())
}

fn validate_send(
    recipients: &[MessagePrincipal],
    request: &SendMessageRequest,
) -> Result<(), MessageStoreError> {
    if request.body.trim().is_empty() || request.body.len() > MAX_MESSAGE_BODY_BYTES {
        return Err(MessageStoreError::contract(
            "message_body_invalid",
            format!("message body must contain 1-{MAX_MESSAGE_BODY_BYTES} bytes"),
        ));
    }
    if recipients.is_empty() || recipients.len() > MAX_MESSAGE_RECIPIENTS {
        return Err(MessageStoreError::contract(
            "message_recipients_invalid",
            format!("message must have 1-{MAX_MESSAGE_RECIPIENTS} recipients"),
        ));
    }
    if request.context_ids.len() > MAX_MESSAGE_CONTEXTS {
        return Err(MessageStoreError::contract(
            "message_contexts_invalid",
            format!("message may reference at most {MAX_MESSAGE_CONTEXTS} contexts"),
        ));
    }
    let recipient_keys = recipients
        .iter()
        .map(|principal| (principal.kind, principal.id.as_str()))
        .collect::<HashSet<_>>();
    if recipient_keys.len() != recipients.len() {
        return Err(MessageStoreError::contract(
            "message_recipients_duplicate",
            "message recipients must be unique",
        ));
    }
    let contexts = request.context_ids.iter().collect::<HashSet<_>>();
    if contexts.len() != request.context_ids.len()
        || request
            .context_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 256)
    {
        return Err(MessageStoreError::contract(
            "message_contexts_invalid",
            "message context IDs must be unique, non-empty, and bounded",
        ));
    }
    if let Some(key) = request.idempotency_key.as_deref() {
        if key.is_empty() || key.len() > MAX_MESSAGE_IDEMPOTENCY_KEY_BYTES {
            return Err(MessageStoreError::contract(
                "message_idempotency_key_invalid",
                format!(
                    "message idempotency key must contain 1-{MAX_MESSAGE_IDEMPOTENCY_KEY_BYTES} bytes"
                ),
            ));
        }
    }
    if request
        .expires_at
        .is_some_and(|expires| expires <= Utc::now())
    {
        return Err(MessageStoreError::contract(
            "message_expiry_invalid",
            "message expiry must be in the future",
        ));
    }
    if let Some(source) = &request.external_source {
        validate_external_source(source)?;
    }
    for recipient in recipients {
        validate_workspace_and_principal("resolved", recipient)?;
    }
    Ok(())
}

fn validate_external_source(source: &MessageExternalSource) -> Result<(), MessageStoreError> {
    let fields = [
        source.channel.as_str(),
        source.conversation_id.as_str(),
        source.message_id.as_str(),
    ];
    if fields
        .iter()
        .any(|value| value.is_empty() || value.len() > 512)
        || source
            .account_id
            .as_ref()
            .is_some_and(|value| value.len() > 512)
        || source.metadata.len() > MAX_MESSAGE_EXTERNAL_METADATA_ENTRIES
        || source
            .metadata
            .iter()
            .any(|(key, value)| key.is_empty() || key.len() > 128 || value.len() > 1_024)
    {
        return Err(MessageStoreError::contract(
            "message_external_source_invalid",
            "external source annotation is empty or exceeds its bounds",
        ));
    }
    Ok(())
}

fn validate_page_size(limit: u16) -> Result<(), MessageStoreError> {
    if limit == 0 || limit > MAX_MESSAGE_PAGE_SIZE {
        return Err(MessageStoreError::contract(
            "message_limit_invalid",
            format!("message limit must be between 1 and {MAX_MESSAGE_PAGE_SIZE}"),
        ));
    }
    Ok(())
}

fn validate_ack(request: &AcknowledgeMessagesRequest) -> Result<(), MessageStoreError> {
    if request.delivery_ids.is_empty() || request.delivery_ids.len() > MAX_ACK_DELIVERIES {
        return Err(MessageStoreError::contract(
            "message_ack_invalid",
            format!("acknowledgement must contain 1-{MAX_ACK_DELIVERIES} deliveries"),
        ));
    }
    if request.operation_id.is_empty()
        || request.operation_id.len() > MAX_MESSAGE_IDEMPOTENCY_KEY_BYTES
    {
        return Err(MessageStoreError::contract(
            "message_ack_operation_invalid",
            "acknowledgement operation ID is empty or too long",
        ));
    }
    let ids = request.delivery_ids.iter().collect::<HashSet<_>>();
    if ids.len() != request.delivery_ids.len()
        || request
            .delivery_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 256)
    {
        return Err(MessageStoreError::contract(
            "message_ack_invalid",
            "delivery IDs must be unique, non-empty, and bounded",
        ));
    }
    Ok(())
}

fn validate_import(
    workspace_id: &str,
    request: &ImportMessagesRequest,
) -> Result<(), MessageStoreError> {
    if request.format != "legacy-mail-v1" {
        return Err(MessageStoreError::contract(
            "message_import_format_unsupported",
            "only legacy-mail-v1 imports are supported",
        ));
    }
    if request.operation_id.is_empty()
        || request.operation_id.len() > MAX_MESSAGE_IDEMPOTENCY_KEY_BYTES
        || request.messages.is_empty()
        || request.messages.len() > MAX_IMPORT_MESSAGES
    {
        return Err(MessageStoreError::contract(
            "message_import_invalid",
            format!(
                "import must contain 1-{MAX_IMPORT_MESSAGES} messages and a bounded operation ID"
            ),
        ));
    }
    if request
        .messages
        .iter()
        .any(|message| message.workspace_id != workspace_id)
    {
        return Err(MessageStoreError::contract(
            "message_import_workspace_mismatch",
            "every imported message must belong to the target workspace",
        ));
    }
    Ok(())
}

fn validate_legacy_message(
    workspace_id: &str,
    message: &LegacyMailMessage,
) -> Result<(), MessageStoreError> {
    validate_workspace_and_principal(workspace_id, &message.sender)?;
    if message.message_id.is_empty()
        || message.message_id.len() > 256
        || message.body.trim().is_empty()
        || message.body.len() > MAX_MESSAGE_BODY_BYTES
        || message.recipients.is_empty()
        || message.recipients.len() > MAX_MESSAGE_RECIPIENTS
        || message.context_ids.len() > MAX_MESSAGE_CONTEXTS
    {
        return Err(MessageStoreError::contract(
            "message_import_record_invalid",
            "legacy message record is empty or exceeds Core limits",
        ));
    }
    let recipient_keys = message
        .recipients
        .iter()
        .map(|recipient| (recipient.principal.kind, recipient.principal.id.as_str()))
        .collect::<HashSet<_>>();
    let positions = message
        .recipients
        .iter()
        .map(|recipient| recipient.position)
        .collect::<HashSet<_>>();
    let contexts = message.context_ids.iter().collect::<HashSet<_>>();
    if recipient_keys.len() != message.recipients.len()
        || positions.len() != message.recipients.len()
        || contexts.len() != message.context_ids.len()
    {
        return Err(MessageStoreError::contract(
            "message_import_record_invalid",
            "legacy recipients, positions, and contexts must be unique",
        ));
    }
    for recipient in &message.recipients {
        validate_workspace_and_principal(workspace_id, &recipient.principal)?;
    }
    Ok(())
}

async fn lock_idempotency(
    connection: &mut PgConnection,
    workspace_id: &str,
    principal: &MessagePrincipal,
    key: &str,
) -> Result<(), MessageStoreError> {
    let lock_key = format!(
        "{workspace_id}:{}:{}:{key}",
        principal.kind.as_str(),
        principal.id
    );
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(connection)
        .await?;
    Ok(())
}

async fn ensure_visible(
    connection: &mut PgConnection,
    workspace_id: &str,
    principal: &MessagePrincipal,
    message_id: &str,
) -> Result<(), MessageStoreError> {
    let visible: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM core_messages m WHERE m.workspace_id = $1 \
         AND m.message_id = $2 AND ((m.sender_kind = $3 AND m.sender_id = $4) OR EXISTS(\
            SELECT 1 FROM core_message_deliveries d WHERE d.workspace_id = m.workspace_id \
            AND d.message_id = m.message_id AND d.recipient_kind = $3 AND d.recipient_id = $4)))",
    )
    .bind(workspace_id)
    .bind(message_id)
    .bind(principal.kind.as_str())
    .bind(&principal.id)
    .fetch_one(connection)
    .await?;
    if visible {
        Ok(())
    } else {
        Err(MessageStoreError::contract(
            "message_not_found",
            "message does not exist or is not visible",
        ))
    }
}

async fn load_message(
    connection: &mut PgConnection,
    workspace_id: &str,
    message_id: &str,
) -> Result<CoreMessage, MessageStoreError> {
    let row = sqlx::query(
        "SELECT message_id, workspace_id, sender_kind, sender_id, sender_name, sender_role, \
                body, created_at, expires_at, correlation_id, trace_id, external_source \
         FROM core_messages WHERE workspace_id = $1 AND message_id = $2",
    )
    .bind(workspace_id)
    .bind(message_id)
    .fetch_optional(&mut *connection)
    .await?
    .ok_or_else(|| MessageStoreError::contract("message_not_found", "message does not exist"))?;
    let recipient_rows = sqlx::query(
        "SELECT recipient_kind, recipient_id, recipient_name, recipient_role \
         FROM core_message_deliveries WHERE workspace_id = $1 AND message_id = $2 \
         ORDER BY position",
    )
    .bind(workspace_id)
    .bind(message_id)
    .fetch_all(&mut *connection)
    .await?;
    let context_rows = sqlx::query(
        "SELECT context_message_id FROM core_message_contexts \
         WHERE workspace_id = $1 AND message_id = $2 ORDER BY position",
    )
    .bind(workspace_id)
    .bind(message_id)
    .fetch_all(&mut *connection)
    .await?;
    let external_source = row
        .get::<Option<Value>, _>("external_source")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| MessageStoreError::Corrupt)?;
    Ok(CoreMessage {
        schema_version: MESSAGE_SCHEMA_VERSION,
        message_id: row.get("message_id"),
        workspace_id: row.get("workspace_id"),
        sender: principal_from_columns(
            row.get("sender_kind"),
            row.get("sender_id"),
            row.get("sender_name"),
            row.get("sender_role"),
        )?,
        recipients: recipient_rows
            .into_iter()
            .map(|row| {
                principal_from_columns(
                    row.get("recipient_kind"),
                    row.get("recipient_id"),
                    row.get("recipient_name"),
                    row.get("recipient_role"),
                )
            })
            .collect::<Result<_, _>>()?,
        context_ids: context_rows
            .into_iter()
            .map(|row| row.get("context_message_id"))
            .collect(),
        body: row.get("body"),
        created_at: parse_timestamp(row.get("created_at"))?,
        expires_at: parse_optional_timestamp(row.get("expires_at"))?,
        correlation_id: row.get("correlation_id"),
        trace_id: row.get("trace_id"),
        external_source,
    })
}

fn delivery_from_row(
    row: &PgRow,
    message: CoreMessage,
) -> Result<MessageDelivery, MessageStoreError> {
    Ok(MessageDelivery {
        delivery_id: row.get("delivery_id"),
        recipient: principal_from_columns(
            row.get("recipient_kind"),
            row.get("recipient_id"),
            row.get("recipient_name"),
            row.get("recipient_role"),
        )?,
        created_at: parse_timestamp(row.get("created_at"))?,
        acknowledged_at: parse_optional_timestamp(row.get("acknowledged_at"))?,
        expires_at: parse_optional_timestamp(row.get("expires_at"))?,
        message,
    })
}

async fn delivery_ids(
    connection: &mut PgConnection,
    workspace_id: &str,
    message_id: &str,
) -> Result<Vec<String>, MessageStoreError> {
    Ok(sqlx::query_scalar(
        "SELECT delivery_id FROM core_message_deliveries WHERE workspace_id = $1 \
         AND message_id = $2 ORDER BY position",
    )
    .bind(workspace_id)
    .bind(message_id)
    .fetch_all(connection)
    .await?)
}

async fn unacknowledged_count(
    connection: &mut PgConnection,
    workspace_id: &str,
    principal: &MessagePrincipal,
) -> Result<u64, MessageStoreError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM core_message_deliveries WHERE workspace_id = $1 \
         AND recipient_kind = $2 AND recipient_id = $3 AND acknowledged_at IS NULL \
         AND (expires_at IS NULL OR expires_at > $4)",
    )
    .bind(workspace_id)
    .bind(principal.kind.as_str())
    .bind(&principal.id)
    .bind(Utc::now().to_rfc3339())
    .fetch_one(connection)
    .await?;
    u64::try_from(count).map_err(|_| MessageStoreError::Corrupt)
}

async fn insert_outbox(
    connection: &mut PgConnection,
    envelope: DomainEventEnvelope,
) -> Result<(), MessageStoreError> {
    let value = serde_json::to_value(&envelope).map_err(|_| MessageStoreError::Corrupt)?;
    sqlx::query(
        "INSERT INTO core_message_outbox(event_id, workspace_id, action, envelope, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&envelope.event_id)
    .bind(&envelope.workspace_id)
    .bind(&envelope.action)
    .bind(value)
    .bind(envelope.occurred_at.to_rfc3339())
    .execute(connection)
    .await?;
    Ok(())
}

async fn insert_legacy_message(
    connection: &mut PgConnection,
    message: &LegacyMailMessage,
) -> Result<(), MessageStoreError> {
    sqlx::query(
        "INSERT INTO core_messages(\
            message_id, workspace_id, sender_kind, sender_id, sender_name, sender_role, body, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&message.message_id)
    .bind(&message.workspace_id)
    .bind(message.sender.kind.as_str())
    .bind(&message.sender.id)
    .bind(&message.sender.name)
    .bind(&message.sender.role)
    .bind(&message.body)
    .bind(message.created_at.to_rfc3339())
    .execute(&mut *connection)
    .await?;
    for recipient in &message.recipients {
        let delivery_id = legacy_delivery_id(
            &message.workspace_id,
            &message.message_id,
            &recipient.principal,
        );
        sqlx::query(
            "INSERT INTO core_message_deliveries(\
                delivery_id, message_id, workspace_id, recipient_kind, recipient_id, \
                recipient_name, recipient_role, position, created_at, acknowledged_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(delivery_id)
        .bind(&message.message_id)
        .bind(&message.workspace_id)
        .bind(recipient.principal.kind.as_str())
        .bind(&recipient.principal.id)
        .bind(&recipient.principal.name)
        .bind(&recipient.principal.role)
        .bind(i64::from(recipient.position))
        .bind(message.created_at.to_rfc3339())
        .bind(recipient.read_at.map(|value| value.to_rfc3339()))
        .execute(&mut *connection)
        .await?;
    }
    for (position, context_id) in message.context_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO core_message_contexts(\
                message_id, workspace_id, context_message_id, position) VALUES ($1, $2, $3, $4)",
        )
        .bind(&message.message_id)
        .bind(&message.workspace_id)
        .bind(context_id)
        .bind(position as i64)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

async fn legacy_message_matches(
    connection: &mut PgConnection,
    expected: &LegacyMailMessage,
) -> Result<bool, MessageStoreError> {
    let actual = load_message(connection, &expected.workspace_id, &expected.message_id).await?;
    let mut expected_recipients = expected.recipients.iter().collect::<Vec<_>>();
    expected_recipients.sort_by_key(|recipient| recipient.position);
    if actual.workspace_id != expected.workspace_id
        || actual.message_id != expected.message_id
        || actual.sender != expected.sender
        || actual.recipients
            != expected_recipients
                .iter()
                .map(|recipient| recipient.principal.clone())
                .collect::<Vec<_>>()
        || actual.context_ids != expected.context_ids
        || actual.body != expected.body
        || actual.created_at != expected.created_at
        || actual.expires_at.is_some()
        || actual.correlation_id.is_some()
        || actual.trace_id.is_some()
        || actual.external_source.is_some()
    {
        return Ok(false);
    }
    let rows = sqlx::query(
        "SELECT recipient_kind, recipient_id, recipient_name, recipient_role, position, \
                acknowledged_at FROM core_message_deliveries \
         WHERE workspace_id = $1 AND message_id = $2 ORDER BY position",
    )
    .bind(&expected.workspace_id)
    .bind(&expected.message_id)
    .fetch_all(connection)
    .await?;
    if rows.len() != expected_recipients.len() {
        return Ok(false);
    }
    for (row, expected_recipient) in rows.iter().zip(expected_recipients) {
        let principal = principal_from_columns(
            row.get("recipient_kind"),
            row.get("recipient_id"),
            row.get("recipient_name"),
            row.get("recipient_role"),
        )?;
        let acknowledged_at = parse_optional_timestamp(row.get("acknowledged_at"))?;
        if principal != expected_recipient.principal
            || row.get::<i64, _>("position") != i64::from(expected_recipient.position)
            || acknowledged_at != expected_recipient.read_at
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn legacy_delivery_id(
    workspace_id: &str,
    message_id: &str,
    recipient: &MessagePrincipal,
) -> String {
    let mut digest = Sha256::new();
    digest.update(workspace_id.as_bytes());
    digest.update([0]);
    digest.update(message_id.as_bytes());
    digest.update([0]);
    digest.update(recipient.kind.as_str().as_bytes());
    digest.update([0]);
    digest.update(recipient.id.as_bytes());
    format!("dlv_legacy_{:x}", digest.finalize())
}

fn request_hash(value: &impl Serialize) -> Result<String, MessageStoreError> {
    let encoded = serde_json::to_vec(value).map_err(|_| MessageStoreError::Corrupt)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn principal_from_columns(
    kind: String,
    id: String,
    name: String,
    role: Option<String>,
) -> Result<MessagePrincipal, MessageStoreError> {
    let kind = match kind.as_str() {
        "agent" => MessagePrincipalKind::Agent,
        "human" => MessagePrincipalKind::Human,
        "machine" => MessagePrincipalKind::Machine,
        "service" => MessagePrincipalKind::Service,
        _ => return Err(MessageStoreError::Corrupt),
    };
    Ok(MessagePrincipal {
        kind,
        id,
        name,
        role,
    })
}

fn parse_timestamp(value: String) -> Result<DateTime<Utc>, MessageStoreError> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| MessageStoreError::Corrupt)
}

fn parse_optional_timestamp(
    value: Option<String>,
) -> Result<Option<DateTime<Utc>>, MessageStoreError> {
    value.map(parse_timestamp).transpose()
}

fn encode_cursor(created_at: &DateTime<Utc>, message_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}\n{message_id}", created_at.to_rfc3339()))
}

fn decode_cursor(value: &str) -> Result<(DateTime<Utc>, String), MessageStoreError> {
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        MessageStoreError::contract("message_cursor_invalid", "message cursor is invalid")
    })?;
    let decoded = String::from_utf8(decoded).map_err(|_| {
        MessageStoreError::contract("message_cursor_invalid", "message cursor is invalid")
    })?;
    let (timestamp, message_id) = decoded.split_once('\n').ok_or_else(|| {
        MessageStoreError::contract("message_cursor_invalid", "message cursor is invalid")
    })?;
    if message_id.is_empty() || message_id.len() > 256 {
        return Err(MessageStoreError::contract(
            "message_cursor_invalid",
            "message cursor is invalid",
        ));
    }
    Ok((
        DateTime::parse_from_rfc3339(timestamp)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|_| {
                MessageStoreError::contract("message_cursor_invalid", "message cursor is invalid")
            })?,
        message_id.to_string(),
    ))
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS core_messages (\
        message_id TEXT NOT NULL, workspace_id TEXT NOT NULL, sender_kind TEXT NOT NULL, \
        sender_id TEXT NOT NULL, sender_name TEXT NOT NULL, sender_role TEXT, body TEXT NOT NULL, \
        created_at TEXT NOT NULL, expires_at TEXT, correlation_id TEXT, trace_id TEXT, \
        external_source JSONB, PRIMARY KEY(workspace_id, message_id), \
        CHECK(sender_kind IN ('agent', 'human', 'machine', 'service')))",
    "CREATE INDEX IF NOT EXISTS core_messages_workspace_created \
        ON core_messages(workspace_id, created_at DESC, message_id DESC)",
    "CREATE TABLE IF NOT EXISTS core_message_deliveries (\
        delivery_id TEXT PRIMARY KEY, message_id TEXT NOT NULL, workspace_id TEXT NOT NULL, \
        recipient_kind TEXT NOT NULL, recipient_id TEXT NOT NULL, recipient_name TEXT NOT NULL, \
        recipient_role TEXT, position BIGINT NOT NULL CHECK(position >= 0), created_at TEXT NOT NULL, \
        acknowledged_at TEXT, expires_at TEXT, \
        UNIQUE(workspace_id, message_id, recipient_kind, recipient_id), \
        UNIQUE(workspace_id, message_id, position), \
        FOREIGN KEY(workspace_id, message_id) REFERENCES core_messages(workspace_id, message_id) \
            ON DELETE CASCADE, \
        CHECK(recipient_kind IN ('agent', 'human', 'machine', 'service')))",
    "CREATE INDEX IF NOT EXISTS core_message_deliveries_inbox \
        ON core_message_deliveries(\
            workspace_id, recipient_kind, recipient_id, acknowledged_at, created_at, delivery_id)",
    "CREATE TABLE IF NOT EXISTS core_message_contexts (\
        message_id TEXT NOT NULL, workspace_id TEXT NOT NULL, context_message_id TEXT NOT NULL, \
        position BIGINT NOT NULL CHECK(position >= 0), \
        PRIMARY KEY(workspace_id, message_id, context_message_id), \
        UNIQUE(workspace_id, message_id, position), \
        FOREIGN KEY(workspace_id, message_id) REFERENCES core_messages(workspace_id, message_id) \
            ON DELETE CASCADE, \
        FOREIGN KEY(workspace_id, context_message_id) REFERENCES core_messages(workspace_id, message_id))",
    "CREATE TABLE IF NOT EXISTS core_message_idempotency (\
        workspace_id TEXT NOT NULL, sender_kind TEXT NOT NULL, sender_id TEXT NOT NULL, \
        idempotency_key TEXT NOT NULL, request_hash TEXT NOT NULL, message_id TEXT NOT NULL, \
        created_at TEXT NOT NULL, PRIMARY KEY(workspace_id, sender_kind, sender_id, idempotency_key), \
        FOREIGN KEY(workspace_id, message_id) REFERENCES core_messages(workspace_id, message_id))",
    "CREATE TABLE IF NOT EXISTS core_message_ack_operations (\
        workspace_id TEXT NOT NULL, principal_kind TEXT NOT NULL, principal_id TEXT NOT NULL, \
        operation_id TEXT NOT NULL, request_hash TEXT NOT NULL, response JSONB NOT NULL, \
        created_at TEXT NOT NULL, PRIMARY KEY(workspace_id, principal_kind, principal_id, operation_id))",
    "CREATE TABLE IF NOT EXISTS core_message_import_operations (\
        workspace_id TEXT NOT NULL, operation_id TEXT NOT NULL, request_hash TEXT NOT NULL, \
        response JSONB NOT NULL, created_at TEXT NOT NULL, actor_kind TEXT NOT NULL, actor_id TEXT NOT NULL, \
        PRIMARY KEY(workspace_id, operation_id))",
    "CREATE TABLE IF NOT EXISTS core_message_outbox (\
        event_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, action TEXT NOT NULL, \
        envelope JSONB NOT NULL, created_at TEXT NOT NULL, dispatched_at TEXT, \
        attempts BIGINT NOT NULL DEFAULT 0 CHECK(attempts >= 0))",
    "CREATE INDEX IF NOT EXISTS core_message_outbox_pending \
        ON core_message_outbox(created_at, event_id) WHERE dispatched_at IS NULL",
];

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    async fn test_store() -> MessageStore {
        let database_url = std::env::var("TREER_TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://treer:treer@127.0.0.1:55432/treer_test".to_string());
        let schema = format!("message_test_{}", Uuid::new_v4().simple());
        let setup = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect to PostgreSQL test database");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&setup)
            .await
            .expect("create test schema");
        setup.close().await;
        let search_path = format!("SET search_path TO {schema}");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .expect("connect to isolated schema");
        MessageStore::open(pool).await.expect("open message store")
    }

    fn agent(id: &str) -> MessagePrincipal {
        MessagePrincipal {
            kind: MessagePrincipalKind::Agent,
            id: id.to_string(),
            name: id.to_string(),
            role: None,
        }
    }

    fn request(
        recipients: &[&str],
        contexts: Vec<String>,
        body: &str,
        idempotency_key: &str,
    ) -> SendMessageRequest {
        SendMessageRequest {
            recipients: recipients.iter().map(|value| value.to_string()).collect(),
            context_ids: contexts,
            body: body.to_string(),
            expires_at: None,
            idempotency_key: Some(idempotency_key.to_string()),
            correlation_id: None,
            trace_id: None,
            external_source: None,
        }
    }

    #[tokio::test]
    async fn send_receive_ack_and_idempotency_survive_store_reopen() {
        let store = test_store().await;
        let sender = agent("agent-a");
        let recipient = agent("agent-b");
        let send = request(&["agent-b"], vec![], "hello", "telegram:update:1");
        let first = store
            .send(
                "workspace-a",
                &sender,
                std::slice::from_ref(&recipient),
                &send,
            )
            .await
            .expect("send message");
        assert!(!first.idempotent_replay);
        let replay = store
            .send(
                "workspace-a",
                &sender,
                std::slice::from_ref(&recipient),
                &send,
            )
            .await
            .expect("replay message");
        assert!(replay.idempotent_replay);
        assert_eq!(first.message.message_id, replay.message.message_id);
        assert_eq!(first.delivery_ids, replay.delivery_ids);

        let received = store
            .receive("workspace-a", &recipient, 50, 0)
            .await
            .expect("receive message");
        assert_eq!(received.deliveries.len(), 1);
        let repeated = store
            .receive("workspace-a", &recipient, 50, 0)
            .await
            .expect("repeat receive");
        assert_eq!(received, repeated);

        let ack = AcknowledgeMessagesRequest {
            delivery_ids: first.delivery_ids.clone(),
            operation_id: "ack-1".to_string(),
        };
        let acknowledged = store
            .acknowledge("workspace-a", &recipient, &ack)
            .await
            .expect("ack message");
        assert_eq!(acknowledged.acknowledged_delivery_ids, first.delivery_ids);
        assert_eq!(
            store
                .acknowledge("workspace-a", &recipient, &ack)
                .await
                .expect("replay ack"),
            acknowledged
        );
        assert!(store
            .receive("workspace-a", &recipient, 50, 0)
            .await
            .expect("empty receive")
            .deliveries
            .is_empty());
    }

    #[tokio::test]
    async fn dag_visibility_and_multi_parent_order_are_enforced() {
        let store = test_store().await;
        let alice = agent("alice");
        let bob = agent("bob");
        let carol = agent("carol");
        let root = store
            .send(
                "workspace-a",
                &alice,
                &[bob.clone(), carol.clone()],
                &request(&["bob", "carol"], vec![], "root", "root"),
            )
            .await
            .expect("root");
        let left = store
            .send(
                "workspace-a",
                &bob,
                std::slice::from_ref(&alice),
                &request(
                    &["alice"],
                    vec![root.message.message_id.clone()],
                    "left",
                    "left",
                ),
            )
            .await
            .expect("left branch");
        let right = store
            .send(
                "workspace-a",
                &carol,
                std::slice::from_ref(&alice),
                &request(
                    &["alice"],
                    vec![root.message.message_id.clone()],
                    "right",
                    "right",
                ),
            )
            .await
            .expect("right branch");
        let merge = store
            .send(
                "workspace-a",
                &alice,
                std::slice::from_ref(&bob),
                &request(
                    &["bob"],
                    vec![
                        right.message.message_id.clone(),
                        left.message.message_id.clone(),
                    ],
                    "merge",
                    "merge",
                ),
            )
            .await
            .expect("merge branches");
        assert_eq!(
            merge.message.context_ids,
            [right.message.message_id, left.message.message_id]
        );
        let invisible = store
            .send(
                "workspace-a",
                &carol,
                std::slice::from_ref(&alice),
                &request(
                    &["alice"],
                    vec![merge.message.message_id],
                    "cannot see merge",
                    "hidden",
                ),
            )
            .await
            .expect_err("invisible context must fail");
        assert_eq!(invisible.code(), "message_context_not_found");
    }

    #[tokio::test]
    async fn legacy_ids_are_workspace_scoped_and_conflicts_are_rejected() {
        let store = test_store().await;
        let importer = MessagePrincipal {
            kind: MessagePrincipalKind::Machine,
            id: "machine-a".to_string(),
            name: "machine-a".to_string(),
            role: None,
        };
        let legacy = |workspace_id: &str, body: &str| LegacyMailMessage {
            message_id: "legacy-shared-id".to_string(),
            workspace_id: workspace_id.to_string(),
            sender: agent("sender"),
            recipients: vec![treer_protocol::LegacyMailRecipient {
                principal: agent("recipient"),
                position: 0,
                read_at: None,
            }],
            context_ids: Vec::new(),
            body: body.to_string(),
            created_at: "2026-08-21T12:00:00Z".parse().expect("timestamp"),
        };
        for workspace_id in ["workspace-a", "workspace-b"] {
            let response = store
                .import_legacy_mail(
                    workspace_id,
                    &importer,
                    &ImportMessagesRequest {
                        format: "legacy-mail-v1".to_string(),
                        operation_id: format!("import-{workspace_id}"),
                        messages: vec![legacy(workspace_id, "same legacy body")],
                    },
                )
                .await
                .expect("workspace-scoped legacy import");
            assert_eq!(response.imported, 1);
        }
        assert_eq!(
            store
                .get("workspace-a", &agent("recipient"), "legacy-shared-id")
                .await
                .expect("workspace A message")
                .workspace_id,
            "workspace-a"
        );
        assert_eq!(
            store
                .get("workspace-b", &agent("recipient"), "legacy-shared-id")
                .await
                .expect("workspace B message")
                .workspace_id,
            "workspace-b"
        );

        let conflict = store
            .import_legacy_mail(
                "workspace-a",
                &importer,
                &ImportMessagesRequest {
                    format: "legacy-mail-v1".to_string(),
                    operation_id: "import-conflicting-retry".to_string(),
                    messages: vec![legacy("workspace-a", "different body")],
                },
            )
            .await
            .expect_err("different legacy data must not be silently accepted");
        assert_eq!(conflict.code(), "message_import_conflict");
    }

    #[tokio::test]
    async fn concurrent_send_idempotency_creates_one_message_and_delivery() {
        let store = test_store().await;
        let sender = agent("sender");
        let recipient = agent("recipient");
        let send = request(
            &["recipient"],
            Vec::new(),
            "concurrent message",
            "concurrent-key",
        );
        let (left, right) = tokio::join!(
            store.send(
                "workspace-a",
                &sender,
                std::slice::from_ref(&recipient),
                &send,
            ),
            store.send(
                "workspace-a",
                &sender,
                std::slice::from_ref(&recipient),
                &send,
            )
        );
        let left = left.expect("first concurrent send");
        let right = right.expect("second concurrent send");
        assert_eq!(left.message.message_id, right.message.message_id);
        assert_eq!(left.delivery_ids, right.delivery_ids);
        assert_ne!(left.idempotent_replay, right.idempotent_replay);
        assert_eq!(
            store
                .receive("workspace-a", &recipient, 50, 0)
                .await
                .expect("receive concurrent result")
                .deliveries
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn outbox_events_recover_without_exposing_message_bodies() {
        let store = test_store().await;
        let sender = agent("sender");
        let recipient = agent("recipient");
        let secret_body = "body-that-must-never-enter-an-event";
        store
            .send(
                "workspace-a",
                &sender,
                std::slice::from_ref(&recipient),
                &request(&["recipient"], Vec::new(), secret_body, "outbox-key"),
            )
            .await
            .expect("send message");
        let stored: Value = sqlx::query_scalar(
            "SELECT envelope FROM core_message_outbox WHERE action = 'message.created'",
        )
        .fetch_one(&store.pool)
        .await
        .expect("load pending outbox event");
        assert!(!stored.to_string().contains(secret_body));

        let bus = EventBus::in_process();
        let mut events = bus.subscribe();
        assert_eq!(store.dispatch_pending(&bus).await.expect("dispatch"), 1);
        let event = events.recv().await.expect("receive dispatched event");
        assert_eq!(event.action, "message.created");
        assert!(!serde_json::to_string(&event)
            .expect("encode dispatched event")
            .contains(secret_body));
        assert_eq!(store.dispatch_pending(&bus).await.expect("redispatch"), 0);
    }

    #[test]
    fn cursors_round_trip_and_reject_malformed_values() {
        let timestamp: DateTime<Utc> = "2026-08-21T12:00:00Z".parse().expect("timestamp");
        let cursor = encode_cursor(&timestamp, "msg_1");
        assert_eq!(
            decode_cursor(&cursor).expect("decode cursor"),
            (timestamp, "msg_1".to_string())
        );
        assert_eq!(
            decode_cursor("not-base64")
                .expect_err("invalid cursor")
                .code(),
            "message_cursor_invalid"
        );
    }
}
