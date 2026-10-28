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
const MAX_MESSAGE_ID_BYTES: usize = 256;
const MAX_MESSAGE_SNAPSHOT_NAME_BYTES: usize = 256;
const MAX_MESSAGE_SNAPSHOT_ROLE_BYTES: usize = 128;
const MAX_MESSAGE_LINEAGE_ID_BYTES: usize = 256;

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
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(2307958)")
            .execute(&mut *transaction)
            .await?;
        for statement in SCHEMA {
            sqlx::query(statement).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
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

    #[cfg(test)]
    pub async fn send(
        &self,
        workspace_id: &str,
        sender: &MessagePrincipal,
        recipients: &[MessagePrincipal],
        request: &SendMessageRequest,
    ) -> Result<SendMessageResponse, MessageStoreError> {
        self.send_with_policy_revision(workspace_id, sender, recipients, request, None)
            .await
    }

    pub async fn send_with_policy_revision(
        &self,
        workspace_id: &str,
        sender: &MessagePrincipal,
        recipients: &[MessagePrincipal],
        request: &SendMessageRequest,
        policy_revision: Option<u64>,
    ) -> Result<SendMessageResponse, MessageStoreError> {
        validate_workspace_and_principal(workspace_id, sender)?;
        validate_send(recipients, request)?;
        let subtree_addressed = sender.app_endpoint.is_some()
            || recipients
                .iter()
                .any(|recipient| recipient.app_endpoint.is_some());
        let fingerprint_recipients = recipients
            .iter()
            .cloned()
            .map(|mut recipient| {
                recipient.path = None;
                // Subtree requests are new wire behavior. Bind their retry to
                // stable addresses, not mutable parent display snapshots; keep
                // legacy root fingerprints byte-compatible with prior sends.
                if subtree_addressed {
                    recipient.name.clear();
                    recipient.role = None;
                }
                recipient
            })
            .collect::<Vec<_>>();
        let request_hash = request_hash(&SendFingerprint {
            sender_endpoint: sender.app_endpoint.as_deref(),
            recipients: &fingerprint_recipients,
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
                created_at, expires_at, correlation_id, trace_id, external_source, sender_path, sender_endpoint) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
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
        .bind(&sender.path)
        .bind(&sender.app_endpoint)
        .execute(&mut *connection)
        .await?;

        let mut delivery_ids = Vec::with_capacity(recipients.len());
        for (position, recipient) in recipients.iter().enumerate() {
            let delivery_id = format!("dlv_{}", Uuid::new_v4().simple());
            sqlx::query(
                "INSERT INTO core_message_deliveries(\
                    delivery_id, message_id, workspace_id, recipient_kind, recipient_id, \
                    recipient_name, recipient_role, position, created_at, expires_at, recipient_path, recipient_endpoint) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
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
            .bind(&recipient.path)
            .bind(recipient.app_endpoint.as_deref().unwrap_or(""))
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
                workspace_revision: policy_revision,
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
        self.list_app_conversation(workspace_id, principal, before, limit, None)
            .await
    }

    pub async fn list_app_conversation(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        before: Option<&str>,
        limit: u16,
        address: Option<(&str, &str)>,
    ) -> Result<MessagePage, MessageStoreError> {
        validate_page_size(limit)?;
        let cursor = before.map(decode_cursor).transpose()?;
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query(
                "SELECT m.message_id, m.created_at FROM core_messages m \
                 WHERE m.workspace_id = $1 AND (\
                    (m.sender_kind = $2 AND m.sender_id = $3) OR EXISTS(\
                        SELECT 1 FROM core_message_deliveries d WHERE d.workspace_id = m.workspace_id \
                        AND d.message_id = m.message_id AND d.recipient_kind = $2 \
                        AND d.recipient_id = $3)) \
                 AND ($4::text IS NULL OR \
                     (m.sender_kind='agent' AND m.sender_id=$4 AND m.sender_endpoint=$5) OR EXISTS(\
                         SELECT 1 FROM core_message_deliveries peer WHERE peer.workspace_id=m.workspace_id \
                         AND peer.message_id=m.message_id AND peer.recipient_kind='agent' \
                         AND peer.recipient_id=$4 AND peer.recipient_endpoint=$5)) \
                 AND ($6::text IS NULL OR m.created_at < $6 OR (m.created_at=$6 AND m.message_id<$7)) \
                 ORDER BY m.created_at DESC, m.message_id DESC LIMIT $8",
            )
            .bind(workspace_id)
            .bind(principal.kind.as_str())
            .bind(&principal.id)
            .bind(address.map(|(owner, _)| owner))
            .bind(address.map(|(_, endpoint)| endpoint))
            .bind(cursor.as_ref().map(|(at, _)| at.to_rfc3339()))
            .bind(cursor.as_ref().map(|(_, id)| id.as_str()))
            .bind(i64::from(limit) + 1)
            .fetch_all(&mut *connection)
            .await?;
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

    #[cfg(test)]
    pub async fn receive(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        limit: u16,
        wait_milliseconds: u64,
    ) -> Result<ReceiveMessagesResponse, MessageStoreError> {
        self.receive_after(workspace_id, principal, limit, wait_milliseconds, None)
            .await
    }

    /// Unacknowledged deliveries in `(created_at, delivery_id)` order. A
    /// principal with `app_endpoint` reads only that endpoint of its own
    /// mailbox; `after` continues a scan and never acknowledges anything.
    pub async fn receive_after(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        limit: u16,
        wait_milliseconds: u64,
        after: Option<&str>,
    ) -> Result<ReceiveMessagesResponse, MessageStoreError> {
        validate_page_size(limit)?;
        validate_endpoint_scope(principal.app_endpoint.as_deref())?;
        let after = after.map(decode_cursor).transpose()?;
        if wait_milliseconds > MAX_MESSAGE_WAIT_MILLISECONDS {
            return Err(MessageStoreError::contract(
                "message_wait_invalid",
                format!(
                    "message wait must be at most {MAX_MESSAGE_WAIT_MILLISECONDS} milliseconds"
                ),
            ));
        }
        let notified = self.changed.notified();
        let first = self
            .receive_now(workspace_id, principal, limit, after.as_ref())
            .await?;
        if !first.deliveries.is_empty() || wait_milliseconds == 0 {
            return Ok(first);
        }
        let _ = tokio::time::timeout(Duration::from_millis(wait_milliseconds), notified).await;
        self.receive_now(workspace_id, principal, limit, after.as_ref())
            .await
    }

    async fn receive_now(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        limit: u16,
        after: Option<&(DateTime<Utc>, String)>,
    ) -> Result<ReceiveMessagesResponse, MessageStoreError> {
        let mut connection = self.pool.acquire().await?;
        let rows = sqlx::query(
            "SELECT delivery_id, message_id, recipient_kind, recipient_id, recipient_name, recipient_path, \
                    recipient_role, recipient_endpoint, created_at, acknowledged_at, expires_at \
             FROM core_message_deliveries WHERE workspace_id = $1 AND recipient_kind = $2 \
             AND recipient_id = $3 AND acknowledged_at IS NULL \
             AND (expires_at IS NULL OR expires_at > $4) \
             AND ($6::text IS NULL OR recipient_endpoint = $6) \
             AND ($7::text IS NULL OR (created_at, delivery_id) > ($7, $8)) \
             ORDER BY created_at, delivery_id LIMIT $5",
        )
        .bind(workspace_id)
        .bind(principal.kind.as_str())
        .bind(&principal.id)
        .bind(Utc::now().to_rfc3339())
        .bind(i64::from(limit))
        .bind(&principal.app_endpoint)
        .bind(after.map(|(time, _)| time.to_rfc3339()))
        .bind(after.map(|(_, id)| id.as_str()))
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
        // The aggregate legacy poll keeps its original wire shape.
        let next_cursor = if principal.app_endpoint.is_some() || after.is_some() {
            deliveries
                .last()
                .map(|delivery| encode_cursor(&delivery.created_at, &delivery.delivery_id))
        } else {
            None
        };
        Ok(ReceiveMessagesResponse {
            next_cursor,
            deliveries,
            remaining_unacknowledged,
        })
    }

    #[cfg(test)]
    pub async fn acknowledge(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        request: &AcknowledgeMessagesRequest,
    ) -> Result<AcknowledgeMessagesResponse, MessageStoreError> {
        self.acknowledge_with_policy_revision(workspace_id, principal, request, None)
            .await
    }

    pub async fn acknowledge_with_policy_revision(
        &self,
        workspace_id: &str,
        principal: &MessagePrincipal,
        request: &AcknowledgeMessagesRequest,
        policy_revision: Option<u64>,
    ) -> Result<AcknowledgeMessagesResponse, MessageStoreError> {
        validate_ack(request)?;
        validate_endpoint_scope(request.app_endpoint.as_deref())?;
        // An endpoint-scoped ACK is a different operation from an unscoped one.
        let request_hash = match request.app_endpoint.as_deref() {
            Some(endpoint) => request_hash(&(endpoint, &request.delivery_ids))?,
            None => request_hash(&request.delivery_ids)?,
        };
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
                 AND recipient_id = $4 AND ($5::text IS NULL OR recipient_endpoint = $5) FOR UPDATE",
            )
            .bind(workspace_id)
            .bind(delivery_id)
            .bind(principal.kind.as_str())
            .bind(&principal.id)
            .bind(&request.app_endpoint)
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
                    workspace_revision: policy_revision,
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
            if event_bus.publish_confirmed(envelope).await.is_err() {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_endpoint: Option<&'a str>,
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
    if workspace_id.is_empty()
        || workspace_id.len() > MAX_MESSAGE_ID_BYTES
        || principal.id.is_empty()
        || workspace_id.chars().any(char::is_control)
        || principal.id.chars().any(char::is_control)
    {
        return Err(MessageStoreError::contract(
            "message_principal_invalid",
            "workspace and principal IDs must be non-empty and bounded",
        ));
    }
    if principal.id.len() > MAX_MESSAGE_ID_BYTES
        || principal.app_endpoint.as_deref().is_some_and(|key| {
            principal.kind != MessagePrincipalKind::Agent
                || !treer_protocol::valid_message_app_endpoint(key)
        })
        || principal.name.is_empty()
        || principal.name.len() > MAX_MESSAGE_SNAPSHOT_NAME_BYTES
        || principal.path.as_ref().is_some_and(|path| {
            path.len() > 4096 || !path.starts_with('/') || path.chars().any(char::is_control)
        })
        || principal.name.chars().any(char::is_control)
        || principal.role.as_ref().is_some_and(|role| {
            role.is_empty()
                || role.len() > MAX_MESSAGE_SNAPSHOT_ROLE_BYTES
                || role.chars().any(char::is_control)
        })
    {
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
    if recipients.len() != request.recipients.len()
        || request
            .recipients
            .iter()
            .any(|target| target.trim().is_empty() || target.len() > MAX_MESSAGE_ID_BYTES)
    {
        return Err(MessageStoreError::contract(
            "message_recipients_invalid",
            "resolved recipients do not match the bounded request targets",
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
        .map(|principal| {
            (
                principal.kind,
                principal.id.as_str(),
                principal.app_endpoint.as_deref(),
            )
        })
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
    for (value, label) in [
        (request.correlation_id.as_deref(), "correlation"),
        (request.trace_id.as_deref(), "trace"),
    ] {
        if value.is_some_and(|value| {
            value.is_empty()
                || value.len() > MAX_MESSAGE_LINEAGE_ID_BYTES
                || value.chars().any(char::is_control)
        }) {
            return Err(MessageStoreError::contract(
                "message_lineage_invalid",
                format!("message {label} ID is empty or exceeds its bounds"),
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

fn validate_endpoint_scope(endpoint: Option<&str>) -> Result<(), MessageStoreError> {
    match endpoint {
        Some(endpoint) if !treer_protocol::valid_message_app_endpoint(endpoint) => Err(
            MessageStoreError::contract("message_endpoint_invalid", "invalid App endpoint"),
        ),
        _ => Ok(()),
    }
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
    if message.sender.app_endpoint.is_some()
        || message
            .recipients
            .iter()
            .any(|recipient| recipient.principal.app_endpoint.is_some())
    {
        return Err(MessageStoreError::contract(
            "message_import_record_invalid",
            "legacy imports cannot assert App subtree identities",
        ));
    }
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
        "SELECT message_id, workspace_id, sender_kind, sender_id, sender_name, sender_role, sender_path, sender_endpoint, \
                body, created_at, expires_at, correlation_id, trace_id, external_source \
         FROM core_messages WHERE workspace_id = $1 AND message_id = $2",
    )
    .bind(workspace_id)
    .bind(message_id)
    .fetch_optional(&mut *connection)
    .await?
    .ok_or_else(|| MessageStoreError::contract("message_not_found", "message does not exist"))?;
    let recipient_rows = sqlx::query(
        "SELECT recipient_kind, recipient_id, recipient_name, recipient_path, recipient_role, recipient_endpoint \
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
            row.get("sender_path"),
            row.get("sender_endpoint"),
        )?,
        recipients: recipient_rows
            .into_iter()
            .map(|row| {
                principal_from_columns(
                    row.get("recipient_kind"),
                    row.get("recipient_id"),
                    row.get("recipient_name"),
                    row.get("recipient_role"),
                    row.get("recipient_path"),
                    Some(row.get("recipient_endpoint")),
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
            row.get("recipient_path"),
            Some(row.get("recipient_endpoint")),
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
         AND (expires_at IS NULL OR expires_at > $4) \
         AND ($5::text IS NULL OR recipient_endpoint = $5)",
    )
    .bind(workspace_id)
    .bind(principal.kind.as_str())
    .bind(&principal.id)
    .bind(Utc::now().to_rfc3339())
    .bind(&principal.app_endpoint)
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
            message_id, workspace_id, sender_kind, sender_id, sender_name, sender_role, body, created_at, sender_path) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&message.message_id)
    .bind(&message.workspace_id)
    .bind(message.sender.kind.as_str())
    .bind(&message.sender.id)
    .bind(&message.sender.name)
    .bind(&message.sender.role)
    .bind(&message.body)
    .bind(message.created_at.to_rfc3339())
    .bind(&message.sender.path)
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
                recipient_name, recipient_role, position, created_at, acknowledged_at, recipient_path) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
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
        .bind(&recipient.principal.path)
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
        "SELECT recipient_kind, recipient_id, recipient_name, recipient_path, recipient_role, recipient_endpoint, position, \
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
            row.get("recipient_path"),
            Some(row.get("recipient_endpoint")),
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
    path: Option<String>,
    app_endpoint: Option<String>,
) -> Result<MessagePrincipal, MessageStoreError> {
    let kind = match kind.as_str() {
        "agent" => MessagePrincipalKind::Agent,
        "human" => MessagePrincipalKind::Human,
        "machine" => MessagePrincipalKind::Machine,
        "service" => MessagePrincipalKind::Service,
        _ => return Err(MessageStoreError::Corrupt),
    };
    Ok(MessagePrincipal {
        app_endpoint: app_endpoint.filter(|key| !key.is_empty()),
        path,
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
    "ALTER TABLE core_messages ADD COLUMN IF NOT EXISTS sender_path TEXT",
    "ALTER TABLE core_message_deliveries ADD COLUMN IF NOT EXISTS recipient_path TEXT",
    "ALTER TABLE core_messages ADD COLUMN IF NOT EXISTS sender_endpoint TEXT",
    "ALTER TABLE core_message_deliveries ADD COLUMN IF NOT EXISTS recipient_endpoint TEXT NOT NULL DEFAULT ''",
    "DO $$ DECLARE c RECORD; BEGIN FOR c IN SELECT conname FROM pg_constraint WHERE conrelid='core_message_deliveries'::regclass AND contype='u' AND pg_get_constraintdef(oid)='UNIQUE (workspace_id, message_id, recipient_kind, recipient_id)' LOOP EXECUTE format('ALTER TABLE core_message_deliveries DROP CONSTRAINT %I', c.conname); END LOOP; END $$",
    "CREATE UNIQUE INDEX IF NOT EXISTS core_message_delivery_address ON core_message_deliveries(workspace_id,message_id,recipient_kind,recipient_id,recipient_endpoint)",
];

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    const WS: &str = "workspace-a";

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
            .unwrap();
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
            .unwrap();
        MessageStore::open(pool).await.unwrap()
    }

    fn principal(kind: MessagePrincipalKind, id: &str) -> MessagePrincipal {
        MessagePrincipal {
            app_endpoint: None,
            path: None,
            role: (kind == MessagePrincipalKind::Human).then(|| "member".to_string()),
            kind,
            id: id.to_string(),
            name: id.to_string(),
        }
    }

    fn agent(id: &str) -> MessagePrincipal {
        principal(MessagePrincipalKind::Agent, id)
    }

    fn human(id: &str) -> MessagePrincipal {
        principal(MessagePrincipalKind::Human, id)
    }

    fn request(
        recipients: &[&str],
        contexts: Vec<String>,
        body: &str,
        idempotency_key: &str,
    ) -> SendMessageRequest {
        SendMessageRequest {
            sender_endpoint: None,
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

    /// Sends `body` (idempotency key = body) from `from` to `to` in [`WS`].
    async fn send(
        store: &MessageStore,
        from: &MessagePrincipal,
        to: &[&MessagePrincipal],
        contexts: &[&str],
        body: &str,
    ) -> Result<SendMessageResponse, MessageStoreError> {
        let ids: Vec<&str> = to.iter().map(|p| p.id.as_str()).collect();
        let recipients: Vec<MessagePrincipal> = to.iter().map(|p| (*p).clone()).collect();
        let contexts = contexts.iter().map(|c| c.to_string()).collect();
        store
            .send(WS, from, &recipients, &request(&ids, contexts, body, body))
            .await
    }

    async fn inbox(store: &MessageStore, who: &MessagePrincipal) -> Vec<String> {
        store
            .receive(WS, who, 50, 0)
            .await
            .unwrap()
            .deliveries
            .into_iter()
            .map(|d| d.delivery_id)
            .collect()
    }

    fn ack(ids: &[&String], operation_id: &str) -> AcknowledgeMessagesRequest {
        AcknowledgeMessagesRequest {
            app_endpoint: None,
            delivery_ids: ids.iter().map(|id| id.to_string()).collect(),
            operation_id: operation_id.to_string(),
        }
    }

    async fn count(store: &MessageStore, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&store.pool)
            .await
            .unwrap()
    }

    fn importer() -> MessagePrincipal {
        principal(MessagePrincipalKind::Machine, "machine-a")
    }

    fn legacy(id: &str, workspace_id: &str, contexts: &[&str], body: &str) -> LegacyMailMessage {
        LegacyMailMessage {
            message_id: id.to_string(),
            workspace_id: workspace_id.to_string(),
            sender: agent("sender"),
            recipients: vec![treer_protocol::LegacyMailRecipient {
                principal: agent("recipient"),
                position: 0,
                read_at: None,
            }],
            context_ids: contexts.iter().map(|c| c.to_string()).collect(),
            body: body.to_string(),
            created_at: "2026-08-21T12:00:00Z".parse().unwrap(),
        }
    }

    fn import(operation_id: &str, messages: Vec<LegacyMailMessage>) -> ImportMessagesRequest {
        ImportMessagesRequest {
            format: "legacy-mail-v1".to_string(),
            operation_id: operation_id.to_string(),
            messages,
        }
    }

    #[tokio::test]
    async fn send_receive_ack_and_idempotency_survive_store_reopen() {
        let store = test_store().await;
        let mut sender = agent("agent-a");
        sender.path = Some("/team-a/sender".into());
        let mut recipient = agent("agent-b");
        recipient.path = Some("/team-b/reviewer".into());
        let first = send(&store, &sender, &[&recipient], &[], "hello")
            .await
            .unwrap();
        assert!(!first.idempotent_replay);
        // Replays return the original message, and paths stay immutable snapshots.
        recipient.path = Some("/moved/reviewer".into());
        let reopened = MessageStore::open(store.pool.clone()).await.unwrap();
        let replay = send(&reopened, &sender, &[&recipient], &[], "hello")
            .await
            .unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(first.message.message_id, replay.message.message_id);
        assert_eq!(first.delivery_ids, replay.delivery_ids);
        assert_eq!(
            replay.message.recipients[0].path.as_deref(),
            Some("/team-b/reviewer")
        );
        assert_eq!(
            replay.message.sender.path.as_deref(),
            Some("/team-a/sender")
        );

        // Receive is repeatable until acknowledged; ack replays are stable.
        let received = reopened.receive(WS, &recipient, 50, 0).await.unwrap();
        assert_eq!(received.deliveries.len(), 1);
        assert_eq!(
            received.deliveries[0].recipient.path.as_deref(),
            Some("/team-b/reviewer")
        );
        assert_eq!(
            received,
            reopened.receive(WS, &recipient, 50, 0).await.unwrap()
        );
        let ack = ack(&first.delivery_ids.iter().collect::<Vec<_>>(), "ack-1");
        let acknowledged = reopened.acknowledge(WS, &recipient, &ack).await.unwrap();
        assert_eq!(acknowledged.acknowledged_delivery_ids, first.delivery_ids);
        let again = MessageStore::open(store.pool.clone()).await.unwrap();
        assert_eq!(
            again.acknowledge(WS, &recipient, &ack).await.unwrap(),
            acknowledged
        );
        assert!(inbox(&again, &recipient).await.is_empty());
    }

    #[tokio::test]
    async fn concurrent_send_idempotency_creates_one_message_and_delivery() {
        let store = test_store().await;
        let (sender, recipient) = (agent("sender"), agent("recipient"));
        let to = [&recipient];
        let (left, right) = tokio::join!(
            send(&store, &sender, &to, &[], "concurrent"),
            send(&store, &sender, &to, &[], "concurrent"),
        );
        let (left, right) = (left.unwrap(), right.unwrap());
        assert_eq!(left.message.message_id, right.message.message_id);
        assert_eq!(left.delivery_ids, right.delivery_ids);
        assert_ne!(left.idempotent_replay, right.idempotent_replay);
        assert_eq!(inbox(&store, &recipient).await.len(), 1);
    }

    #[tokio::test]
    async fn endpoint_inbox_receive_after_and_ack_are_scoped_to_the_owner() {
        let store = test_store().await;
        // A Proxy that ran the old typed-inbox canary left this column behind.
        sqlx::query("ALTER TABLE core_messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'message' CHECK (kind IN ('message','prompt'))")
            .execute(&store.pool)
            .await
            .unwrap();
        let sender = agent("sender");
        let owner = agent("app-owner");
        let at = |base: &MessagePrincipal, endpoint: &str| {
            let mut scoped = base.clone();
            scoped.app_endpoint = Some(endpoint.into());
            scoped
        };
        let (alice, bob) = (at(&owner, "alice"), at(&owner, "bob"));
        let first = store
            .send(
                "workspace-a",
                &sender,
                std::slice::from_ref(&alice),
                &request(&["app-owner/alice"], vec![], "one", "k1"),
            )
            .await
            .unwrap();
        let both = store
            .send(
                "workspace-a",
                &sender,
                &[alice.clone(), bob.clone()],
                &request(&["app-owner/alice", "app-owner/bob"], vec![], "two", "k2"),
            )
            .await
            .unwrap();
        // The aggregate poll keeps its legacy wire shape.
        let aggregate = store.receive("workspace-a", &owner, 10, 0).await.unwrap();
        assert_eq!(aggregate.deliveries.len(), 3);
        assert!(serde_json::to_value(&aggregate)
            .unwrap()
            .get("next_cursor")
            .is_none());

        let page = store.receive("workspace-a", &alice, 1, 0).await.unwrap();
        assert_eq!(page.deliveries[0].delivery_id, first.delivery_ids[0]);
        assert_eq!(
            page.deliveries[0].recipient.app_endpoint.as_deref(),
            Some("alice")
        );
        assert_eq!(page.remaining_unacknowledged, 2);
        assert_eq!(
            page,
            store.receive("workspace-a", &alice, 1, 0).await.unwrap(),
            "poll does not consume"
        );
        let next = store
            .receive_after("workspace-a", &alice, 1, 0, page.next_cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(next.deliveries[0].delivery_id, both.delivery_ids[0]);
        let end = store
            .receive_after("workspace-a", &alice, 1, 0, next.next_cursor.as_deref())
            .await
            .unwrap();
        assert!(end.deliveries.is_empty() && end.next_cursor.is_none());
        assert_eq!(
            end.remaining_unacknowledged, 2,
            "a cursor never acknowledges"
        );
        assert_eq!(
            store
                .receive("workspace-a", &bob, 10, 0)
                .await
                .unwrap()
                .deliveries
                .len(),
            1
        );

        // Another workload never sees or acknowledges this owner's endpoint.
        let stranger = at(&agent("other-app"), "alice");
        assert!(store
            .receive("workspace-a", &stranger, 10, 0)
            .await
            .unwrap()
            .deliveries
            .is_empty());
        let scoped_ack =
            |ids: Vec<String>, endpoint: &str, operation: &str| AcknowledgeMessagesRequest {
                app_endpoint: Some(endpoint.into()),
                delivery_ids: ids,
                operation_id: operation.into(),
            };
        let foreign = scoped_ack(first.delivery_ids.clone(), "alice", "foreign");
        assert_eq!(
            store
                .acknowledge("workspace-a", &agent("other-app"), &foreign)
                .await
                .unwrap_err()
                .code(),
            "message_delivery_not_found"
        );
        // A batch with another endpoint's delivery fails entirely.
        let mixed = scoped_ack(
            vec![first.delivery_ids[0].clone(), both.delivery_ids[1].clone()],
            "alice",
            "mixed",
        );
        assert_eq!(
            store
                .acknowledge("workspace-a", &owner, &mixed)
                .await
                .unwrap_err()
                .code(),
            "message_delivery_not_found"
        );
        assert_eq!(
            store
                .receive("workspace-a", &alice, 10, 0)
                .await
                .unwrap()
                .deliveries
                .len(),
            2
        );

        let ack = scoped_ack(first.delivery_ids.clone(), "alice", "ack-alice");
        let acknowledged = store
            .acknowledge("workspace-a", &owner, &ack)
            .await
            .unwrap();
        assert_eq!(acknowledged.acknowledged_delivery_ids, first.delivery_ids);
        assert_eq!(
            store
                .acknowledge("workspace-a", &owner, &ack)
                .await
                .unwrap(),
            acknowledged,
            "retry replays"
        );
        assert_eq!(
            store
                .acknowledge(
                    "workspace-a",
                    &owner,
                    &scoped_ack(first.delivery_ids.clone(), "bob", "ack-alice")
                )
                .await
                .unwrap_err()
                .code(),
            "message_ack_idempotency_conflict"
        );
        let unscoped = AcknowledgeMessagesRequest {
            app_endpoint: None,
            ..ack.clone()
        };
        assert_eq!(
            store
                .acknowledge("workspace-a", &owner, &unscoped)
                .await
                .unwrap_err()
                .code(),
            "message_ack_idempotency_conflict"
        );
        let remaining = store.receive("workspace-a", &alice, 10, 0).await.unwrap();
        assert_eq!(remaining.deliveries.len(), 1);
        assert_eq!(remaining.remaining_unacknowledged, 1);
        assert_eq!(
            store
                .receive("workspace-a", &bob, 10, 0)
                .await
                .unwrap()
                .deliveries
                .len(),
            1
        );
        assert_eq!(
            store
                .receive("workspace-a", &at(&owner, "bad endpoint"), 10, 0)
                .await
                .unwrap_err()
                .code(),
            "message_endpoint_invalid"
        );
    }

    #[tokio::test]
    async fn app_tree_deliveries_and_logical_sender_survive_ack_reopen_and_retry() {
        let store = test_store().await;
        // Exercise upgrade from the pre-subaddress schema and its old unique
        // constraint, not only CREATE TABLE on an empty database.
        sqlx::raw_sql("ALTER TABLE core_messages DROP COLUMN sender_endpoint; ALTER TABLE core_message_deliveries DROP COLUMN recipient_endpoint; ALTER TABLE core_message_deliveries ADD CONSTRAINT legacy_recipient_unique UNIQUE(workspace_id,message_id,recipient_kind,recipient_id)")
            .execute(&store.pool).await.unwrap();
        let store = MessageStore::open(store.pool.clone()).await.unwrap();
        let sender = human("human-a");
        let owner = agent("app-a");
        let endpoint = |path: &str| MessagePrincipal {
            app_endpoint: Some(path.into()),
            ..owner.clone()
        };
        let (left, right) = (endpoint("team/tools/agent1"), endpoint("team/tools/agent2"));
        let input = request(
            &[
                "app-a/team/tools/agent1",
                "app-a/team/tools/agent2",
                "app-a",
            ],
            vec![],
            "hello",
            "subtrees",
        );
        let sent = store
            .send(
                WS,
                &sender,
                &[left.clone(), right.clone(), owner.clone()],
                &input,
            )
            .await
            .unwrap();
        assert_eq!(sent.delivery_ids.len(), 3);
        let received = store.receive(WS, &owner, 10, 0).await.unwrap();
        assert_eq!(received.deliveries.len(), 3);
        let first = received
            .deliveries
            .iter()
            .find(|d| d.delivery_id == sent.delivery_ids[0])
            .unwrap();
        assert_eq!(first.recipient.app_endpoint, left.app_endpoint);

        let reopened = MessageStore::open(store.pool.clone()).await.unwrap();
        let moved_left = MessagePrincipal {
            name: "renamed-parent".into(),
            path: Some("/moved/hub".into()),
            ..left.clone()
        };
        let replay = reopened
            .send(WS, &sender, &[moved_left, right, owner.clone()], &input)
            .await
            .unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.delivery_ids, sent.delivery_ids);
        reopened
            .acknowledge(WS, &owner, &ack(&[&sent.delivery_ids[0]], "ack-tree"))
            .await
            .unwrap();
        assert_eq!(inbox(&reopened, &owner).await.len(), 2);

        // A reply keeps the logical (endpoint) sender; retrying the same key
        // from a different endpoint conflicts; other workspaces see nothing.
        let reply = request(
            &["human-a"],
            vec![sent.message.message_id],
            "reply",
            "reply-tree",
        );
        let answered = reopened
            .send(WS, &left, std::slice::from_ref(&sender), &reply)
            .await
            .unwrap();
        let id = &answered.message.message_id;
        assert_eq!(
            reopened
                .get(WS, &sender, id)
                .await
                .unwrap()
                .sender
                .app_endpoint,
            left.app_endpoint
        );
        let other = endpoint("different");
        assert_eq!(
            reopened
                .send(WS, &other, &[sender], &reply)
                .await
                .unwrap_err()
                .code(),
            "message_idempotency_conflict"
        );
        assert_eq!(
            reopened
                .get("other-workspace", &owner, id)
                .await
                .unwrap_err()
                .code(),
            "message_not_found"
        );
    }

    #[tokio::test]
    async fn multi_recipient_delivery_and_acknowledgement_are_atomic_and_independent() {
        let store = test_store().await;
        let (sender, a, h) = (agent("sender"), agent("agent-r"), human("human-r"));
        let sent = send(&store, &sender, &[&a, &h], &[], "one atomic message")
            .await
            .unwrap();
        assert_eq!(inbox(&store, &a).await, [sent.delivery_ids[0].clone()]);
        assert_eq!(inbox(&store, &h).await, [sent.delivery_ids[1].clone()]);
        store
            .acknowledge(WS, &a, &ack(&[&sent.delivery_ids[0]], "agent-ack"))
            .await
            .unwrap();
        assert!(inbox(&store, &a).await.is_empty());
        assert_eq!(inbox(&store, &h).await.len(), 1);
        // A partially invalid batch rolls back entirely and is not recorded.
        let missing = "dlv-does-not-exist".to_string();
        let failed = store
            .acknowledge(
                WS,
                &h,
                &ack(&[&sent.delivery_ids[1], &missing], "failed-ack"),
            )
            .await
            .unwrap_err();
        assert_eq!(failed.code(), "message_delivery_not_found");
        assert_eq!(inbox(&store, &h).await.len(), 1);
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM core_message_ack_operations WHERE operation_id = 'failed-ack'"
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn context_dag_order_visibility_and_invalid_edges_roll_back() {
        let store = test_store().await;
        let (alice, bob, carol) = (agent("alice"), agent("bob"), agent("carol"));
        let root = send(&store, &alice, &[&bob], &[], "private root body")
            .await
            .unwrap();
        let root_id = root.message.message_id.as_str();
        let left = send(&store, &bob, &[&alice], &[root_id], "left")
            .await
            .unwrap();
        let right = send(&store, &bob, &[&alice, &carol], &[root_id], "right")
            .await
            .unwrap();
        let (left_id, right_id) = (&left.message.message_id, &right.message.message_id);
        // Multi-parent order is preserved as given.
        let merge = send(&store, &alice, &[&bob], &[right_id, left_id], "merge")
            .await
            .unwrap();
        assert_eq!(
            merge.message.context_ids,
            [right_id.clone(), left_id.clone()]
        );
        // An edge never reveals its parent's body to a new recipient.
        assert!(!serde_json::to_string(&right.message)
            .unwrap()
            .contains("private root body"));
        assert_eq!(
            store.get(WS, &carol, root_id).await.unwrap_err().code(),
            "message_not_found"
        );

        let other_ws = store
            .send(
                "workspace-b",
                &alice,
                std::slice::from_ref(&bob),
                &request(&["bob"], vec![], "other", "other"),
            )
            .await
            .unwrap();
        let baseline = count(
            &store,
            "SELECT COUNT(*) FROM core_messages WHERE workspace_id = 'workspace-a'",
        )
        .await;
        let merge_id = merge.message.message_id.as_str();
        for (from, to, contexts, expected) in [
            (
                &alice,
                &bob,
                vec!["msg-missing"],
                "message_context_not_found",
            ),
            (
                &alice,
                &bob,
                vec![other_ws.message.message_id.as_str()],
                "message_context_not_found",
            ),
            (&carol, &alice, vec![root_id], "message_context_not_found"),
            (&carol, &alice, vec![merge_id], "message_context_not_found"),
            (
                &bob,
                &alice,
                vec![root_id, root_id],
                "message_contexts_invalid",
            ),
        ] {
            let error = send(&store, from, &[to], &contexts, &format!("{contexts:?}"))
                .await
                .unwrap_err();
            assert_eq!(error.code(), expected, "{contexts:?}");
        }
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM core_messages WHERE workspace_id = 'workspace-a'"
            )
            .await,
            baseline
        );
    }

    #[tokio::test]
    async fn legacy_import_is_workspace_scoped_topological_and_conflict_checked() {
        let store = test_store().await;
        for ws in ["workspace-a", "workspace-b"] {
            let response = store
                .import_legacy_mail(
                    ws,
                    &importer(),
                    &import(
                        &format!("import-{ws}"),
                        vec![legacy("shared-id", ws, &[], "same")],
                    ),
                )
                .await
                .unwrap();
            assert_eq!(response.imported, 1);
            assert_eq!(
                store
                    .get(ws, &agent("recipient"), "shared-id")
                    .await
                    .unwrap()
                    .workspace_id,
                ws
            );
        }
        let conflict = store
            .import_legacy_mail(
                WS,
                &importer(),
                &import(
                    "retry",
                    vec![legacy("shared-id", WS, &[], "different body")],
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code(), "message_import_conflict");
        let forward = store
            .import_legacy_mail(
                "workspace-import",
                &importer(),
                &import(
                    "forward",
                    vec![
                        legacy("child", "workspace-import", &["parent"], "child"),
                        legacy("parent", "workspace-import", &[], "parent"),
                    ],
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(forward.code(), "message_import_not_topological");
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM core_messages WHERE workspace_id = 'workspace-import'"
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn expiry_preserves_history_while_bounds_reject_oversized_requests() {
        let store = test_store().await;
        let (sender, recipient) = (agent("sender"), agent("recipient"));
        let mut expiring = request(&["recipient"], Vec::new(), "expires", "expires-key");
        expiring.expires_at = Some(Utc::now() + chrono::Duration::hours(1));
        let sent = store
            .send(WS, &sender, std::slice::from_ref(&recipient), &expiring)
            .await
            .unwrap();
        for table in ["core_messages", "core_message_deliveries"] {
            sqlx::query(&format!(
                "UPDATE {table} SET expires_at = '2000-01-01T00:00:00Z' WHERE message_id = $1"
            ))
            .bind(&sent.message.message_id)
            .execute(&store.pool)
            .await
            .unwrap();
        }
        assert!(inbox(&store, &recipient).await.is_empty());
        assert_eq!(
            store
                .get(WS, &recipient, &sent.message.message_id)
                .await
                .unwrap()
                .body,
            "expires"
        );
        assert_eq!(
            store
                .list(WS, &recipient, None, 10)
                .await
                .unwrap()
                .messages
                .len(),
            1
        );

        let one = std::slice::from_ref(&recipient);
        let mut oversized = request(&["recipient"], Vec::new(), "x", "body-bound");
        oversized.body = "x".repeat(MAX_MESSAGE_BODY_BYTES + 1);
        assert_eq!(
            validate_send(one, &oversized).unwrap_err().code(),
            "message_body_invalid"
        );
        let names: Vec<String> = (0..=MAX_MESSAGE_RECIPIENTS)
            .map(|i| format!("r-{i}"))
            .collect();
        let many: Vec<MessagePrincipal> = names.iter().map(|n| agent(n)).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            validate_send(&many, &request(&refs, vec![], "b", "k"))
                .unwrap_err()
                .code(),
            "message_recipients_invalid"
        );
        let contexts = (0..=MAX_MESSAGE_CONTEXTS)
            .map(|i| format!("msg-{i}"))
            .collect();
        assert_eq!(
            validate_send(one, &request(&["recipient"], contexts, "b", "k"))
                .unwrap_err()
                .code(),
            "message_contexts_invalid"
        );
        assert_eq!(
            validate_page_size(0).unwrap_err().code(),
            "message_limit_invalid"
        );
        assert_eq!(
            store
                .receive(WS, &recipient, 1, MAX_MESSAGE_WAIT_MILLISECONDS + 1)
                .await
                .unwrap_err()
                .code(),
            "message_wait_invalid"
        );
    }

    #[tokio::test]
    async fn outbox_is_atomic_body_free_and_dispatches_once() {
        let store = test_store().await;
        let (sender, recipient) = (agent("sender"), agent("recipient"));
        let secret = "body-that-must-stay-out-of-errors-and-events";
        store
            .send_with_policy_revision(
                WS,
                &sender,
                std::slice::from_ref(&recipient),
                &request(&["recipient"], Vec::new(), secret, "revision-key"),
                Some(77),
            )
            .await
            .unwrap();
        let envelope: Value = sqlx::query_scalar(
            "SELECT envelope FROM core_message_outbox WHERE action = 'message.created'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(envelope["workspace_revision"], 77);
        assert!(!envelope.to_string().contains(secret));

        let counts = || async {
            (
                count(&store, "SELECT COUNT(*) FROM core_messages").await,
                count(&store, "SELECT COUNT(*) FROM core_message_outbox").await,
            )
        };
        let before = counts().await;
        let failure = store
            .send(
                WS,
                &sender,
                std::slice::from_ref(&recipient),
                &request(
                    &["recipient"],
                    vec!["missing".to_string()],
                    secret,
                    "failed",
                ),
            )
            .await
            .unwrap_err();
        assert!(!failure.to_string().contains(secret));
        assert_eq!(counts().await, before);

        let bus = EventBus::in_process();
        let mut events = bus.subscribe();
        assert_eq!(store.dispatch_pending(&bus).await.unwrap(), 1);
        let event = events.recv().await.unwrap();
        assert_eq!(event.action, "message.created");
        assert!(!serde_json::to_string(&event).unwrap().contains(secret));
        assert_eq!(store.dispatch_pending(&bus).await.unwrap(), 0);
    }

    #[test]
    fn cursors_round_trip_and_reject_malformed_values() {
        let timestamp: DateTime<Utc> = "2026-08-21T12:00:00Z".parse().unwrap();
        let cursor = encode_cursor(&timestamp, "msg_1");
        assert_eq!(
            decode_cursor(&cursor).unwrap(),
            (timestamp, "msg_1".to_string())
        );
        assert_eq!(
            decode_cursor("not-base64").unwrap_err().code(),
            "message_cursor_invalid"
        );
    }
}
