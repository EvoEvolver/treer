//! Atomic queue completion and confirmed-disposition ACK for negotiated runtime receipt mode.
//! Core deliveries are obtained from this hub's authenticated workspace mailbox;
//! no HTTP body or client-supplied principal may construct this authority.
use crate::{
    core::AuthenticatedDelivery,
    hub::{Directory, Error, Hub},
    protocol::{Channel, Envelope, ExternalSource, Fragment, SenderKind},
    relay::Mailbox,
    telegram::CoreIntent,
    telegram_receipts::{Expected, Receipt, Scope},
    telegram_reply_store::{record_in_transaction, HandoffState, Recorded},
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

/// Completion stage for the negotiated production inbox dispatcher. Its input must come from Core::receive_authenticated, never JSON or
/// generic Mailbox values. No CLI work runs while the hub mutex is held.
pub fn commit_and_ack<M: Mailbox + Directory>(
    hub: &std::sync::Mutex<Hub<M>>,
    core: &crate::core::Core,
    authenticated: &AuthenticatedDelivery,
    intent: &CoreIntent,
    now: i64,
) -> Result<Recorded, Error> {
    let (recorded, disposition) = {
        let mut hub = hub.lock().map_err(|_| Error::Binding)?;
        if !core.is_identity(&hub.own_id) {
            return Err(Error::Binding);
        }
        hub.complete_telegram_receipt_dispatch(authenticated, intent, now, true)?
    }; // Commit returned Ok and the mutex is released before Core/CLI ack.
       // Ack failure keeps the committed result. Redelivery revalidates the exact
       // receipt and retries only its idempotent ack, never external delivery.
    if let Some(pending) = disposition {
        let confirmed = pending.confirm(core)?;
        let mut hub = hub.lock().map_err(|_| Error::Binding)?;
        confirmed.mark(&mut hub.store)?;
    } else {
        core.ack(&authenticated.delivery().delivery_id)?;
    }
    Ok(recorded)
}

/// The receipt body is only a bounded selector. Durable job, scope, source,
/// generation and digest are reconstructed and revalidated by completion.
/// Receipt-shaped invalid input never falls through to ordinary outbound text.
pub(crate) fn dispatch(
    hub: &std::sync::Mutex<Hub<crate::core::Core>>,
    core: &crate::core::Core,
    authenticated: &AuthenticatedDelivery,
    now: i64,
) -> Result<bool, Error> {
    let body = &authenticated.delivery().message.body;
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    match value.get("protocol").and_then(|v| v.as_str()) {
        Some(crate::telegram_receipts::CAPABILITY) => {}
        Some(p) if p.starts_with("treer.telegram.app-asserted-receipts.") => {
            return Err(Error::Binding)
        }
        _ => return Ok(false),
    }
    if body.len() > 4096 {
        return Err(Error::Binding);
    }
    let receipt: Receipt = serde_json::from_value(value).map_err(|_| Error::Binding)?;
    let intent = {
        let h = hub.lock().map_err(|_| Error::Binding)?;
        if receipt.scope.endpoint_id != authenticated.delivery().message.sender.id {
            return Err(Error::Binding);
        }
        let row: Option<(i64,String)> = h.store.db.query_row(
            "SELECT i.id,i.lease_token FROM inbound i JOIN telegram_handoff_modes m ON m.job=i.id AND m.mode='receipt'
             JOIN telegram_sources s ON s.endpoint=i.endpoint AND s.generation=i.generation AND s.external_id=i.external_id
             WHERE i.endpoint=?1 AND i.generation=?2 AND i.recipient=?3
             AND json_extract(s.source,'$.message_id')=?4",
            params![receipt.scope.endpoint_id,receipt.scope.generation,receipt.scope.recipient_id,receipt.native_message_id.to_string()],
            |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let (job, token) = row.ok_or(Error::Binding)?;
        CoreIntent {
            endpoint: receipt.scope.endpoint_id.clone(),
            generation: receipt.scope.generation,
            job,
            token,
            parts: vec![],
        }
    };
    commit_and_ack(hub, core, authenticated, &intent, now)?;
    Ok(true)
}

impl<M: Mailbox + Directory> Hub<M> {
    #[cfg(test)]
    pub(crate) fn complete_test_telegram_receipt(
        &mut self,
        delivery: crate::core::Delivery,
        intent: &CoreIntent,
        now: i64,
    ) -> Result<Recorded, Error> {
        let authenticated = AuthenticatedDelivery::fixture(delivery, &self.workspace);
        self.complete_telegram_receipt(&authenticated, intent, now)
    }

    /// The local correlation/lease comes from prepare_telegram_forward, never
    /// from receipt JSON. This method commits before returning permission to ack.
    /// Conflict is an error AFTER committing a bounded operator-visible pause;
    /// the original queue row remains pending and the Core delivery unacked.
    /// A persisted receipt-mode pin is required; legacy completion rejects it.
    pub fn complete_telegram_receipt(
        &mut self,
        authenticated: &AuthenticatedDelivery,
        intent: &CoreIntent,
        now: i64,
    ) -> Result<Recorded, Error> {
        self.complete_telegram_receipt_dispatch(authenticated, intent, now, false)
            .map(|(recorded, _)| recorded)
    }

    fn complete_telegram_receipt_dispatch<'a>(
        &mut self,
        authenticated: &'a AuthenticatedDelivery,
        intent: &CoreIntent,
        now: i64,
        allow_discard: bool,
    ) -> Result<
        (
            Recorded,
            Option<crate::telegram_disposition_ack::PendingAck<'a>>,
        ),
        Error,
    > {
        let delivery = authenticated.delivery();
        if authenticated.workspace() != self.workspace
            || now < crate::transfers::TOMBSTONE_TTL
            || delivery.recipient.kind != SenderKind::Agent
            || delivery.recipient.id != self.own_id
            || delivery.message.sender.kind != SenderKind::Agent
            || delivery.message.sender.id != intent.endpoint
        {
            return Err(Error::Binding);
        }
        self.store.recheck_private_storage()?;
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        let tx = self
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity: (String, String) = tx.query_row(
            "SELECT agent,workspace FROM hub_identity WHERE singleton=1 AND blocked=0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if identity != (self.own_id.clone(), self.workspace.clone()) {
            return Err(Error::Binding);
        }
        // Re-read authoritative generation, recipient, routing, source and queue
        // completion under the write transaction. Never trust an earlier View,
        // a caller's HandoffState, or absence in the history ring.
        let row = tx.query_row(
            "SELECT i.recipient,i.body,i.external_id,i.pending,i.lease_token,i.core_id,
                    s.source,t.account,t.chat,t.topic
             FROM inbound i JOIN endpoints e ON e.id=i.endpoint AND e.generation=i.generation
             JOIN hub_bindings b ON b.endpoint=e.id AND b.reason!='deleted'
             JOIN telegram_sources s ON s.endpoint=i.endpoint AND s.generation=i.generation AND s.external_id=i.external_id
             JOIN telegram_targets t ON t.endpoint=i.endpoint
             WHERE i.id=?1 AND i.endpoint=?2 AND i.generation=?3 AND i.recipient=e.recipient",
            params![intent.job,intent.endpoint,intent.generation],
            |r| Ok(Handoff {
                recipient:r.get(0)?,body:r.get(1)?,external:r.get(2)?,pending:r.get(3)?,
                lease:r.get(4)?,core_id:r.get(5)?,source:r.get(6)?,account:r.get(7)?,chat:r.get(8)?,topic:r.get(9)?,
            }),
        ).optional()?.ok_or(Error::Binding)?;
        let mode: Option<String> = tx
            .query_row(
                "SELECT mode FROM telegram_handoff_modes WHERE job=?1",
                [intent.job],
                |r| r.get(0),
            )
            .optional()?;
        if mode.as_deref() != Some(crate::telegram::CompletionMode::Receipt.as_str()) {
            return Err(Error::Binding);
        }
        if row.lease.as_deref() != Some(&intent.token) || (!row.pending && row.core_id.is_none()) {
            return Err(Error::Binding);
        }
        let source: ExternalSource =
            serde_json::from_str(&row.source).map_err(|_| Error::Binding)?;
        if !matches!(source.channel, Channel::Telegram)
            || source.account_id != row.account
            || source.conversation_id != format!("{}:{}", row.chat, row.topic)
        {
            return Err(Error::Binding);
        }
        let native_id = source
            .message_id
            .parse::<i64>()
            .map_err(|_| Error::Binding)?;
        let scope = Scope {
            account_id: row.account,
            chat_id: row.chat,
            topic_id: if row.topic == 0 {
                None
            } else {
                Some(row.topic)
            },
            endpoint_id: intent.endpoint.clone(),
            generation: intent.generation,
            recipient_id: row.recipient.clone(),
        };
        let job_id = format!(
            "ep-in-{}",
            crate::protocol::digest_id(
                "inbound-delivery",
                &intent.endpoint,
                intent.generation as u64,
                &row.external,
            )
        );
        let envelope = Envelope::Deliver {
            receipt_scope: Some(scope.clone()),
            job_id: job_id.clone(),
            recipient: row.recipient,
            body: row.body,
            context_ids: serde_json::from_str(&tx.query_row("SELECT contexts FROM telegram_reply_contexts WHERE endpoint=?1 AND generation=?2 AND external_id=?3",params![intent.endpoint,intent.generation,row.external],|r|r.get::<_,String>(0)).optional()?.unwrap_or_else(||"[]".into())).map_err(|_|Error::Binding)?,
            source,
        };
        let frames = envelope
            .fragments(&intent.endpoint, intent.generation as u64)
            .map_err(|_| Error::Binding)?;
        let first = frames.first().ok_or(Error::Binding)?;
        let digest = Fragment::parse(first)
            .map_err(|_| Error::Binding)?
            .payload_digest;
        let receipt = Receipt::parse_for(
            delivery.message.body.as_bytes(),
            &delivery.message.sender.id,
            Expected {
                scope: &scope,
                job_id: &job_id,
                payload_digest: &digest,
                native_message_id: native_id,
            },
        )
        .map_err(|_| Error::Binding)?;
        // Terminal storage outcomes alone never authorize ack. The bounded
        // mapping ring may be evicted, but the completed job retains its final
        // Core ID. A changed assertion must not be acknowledged or resurrect
        // history, including after restart or mapping eviction.
        if !row.pending && row.core_id.as_deref() != Some(&receipt.final_core_message_id) {
            let disposition_identity =
                crate::telegram_disposition_identity::Identity::from_authenticated(
                    authenticated,
                    &self.workspace,
                    &self.own_id,
                    intent.job,
                    Expected {
                        scope: &scope,
                        job_id: &job_id,
                        payload_digest: &digest,
                        native_message_id: native_id,
                    },
                )?;
            let admission = crate::telegram_disposition_admission::record_and_hold_in_transaction(
                &tx,
                &disposition_identity,
                now,
            )?;
            tx.commit()?;
            if !allow_discard
                || !matches!(
                    admission,
                    crate::telegram_disposition_admission::Admission::Inserted
                        | crate::telegram_disposition_admission::Admission::Replay
                )
            {
                return Err(Error::Binding);
            }
            // The committed disposition is only local evidence. Prepare a
            // borrowed exact-delivery capability; the caller releases this Hub
            // before confirming Core's ACK and re-locks only to mark its ordinal.
            let pending = crate::telegram_disposition_ack::prepare(
                &mut self.store,
                authenticated,
                intent.job,
                Expected {
                    scope: &scope,
                    job_id: &job_id,
                    payload_digest: &digest,
                    native_message_id: native_id,
                },
            )?;
            return Ok((Recorded::DiscardedTerminal, Some(pending)));
        }
        let state = if row.pending {
            HandoffState::AwaitingReceipt
        } else {
            HandoffState::Terminal
        };
        let result = record_in_transaction(&tx, &receipt, state, now)?;
        if result == Recorded::ConflictHeld {
            // Existing bounded inbound row is the hold; no body/ID is copied to
            // another queue. View.reason exposes recovery, not a new authority.
            tx.execute(
                "UPDATE endpoints SET paused=1 WHERE id=?1",
                [&intent.endpoint],
            )?;
            tx.execute(
                "UPDATE hub_bindings SET revision=revision+1,reason='receipt_conflict'
                WHERE endpoint=?1 AND reason!='receipt_conflict'",
                [&intent.endpoint],
            )?;
            tx.commit()?;
            return Err(Error::ReceiptConflict);
        }
        if row.pending {
            tx.execute(
                "UPDATE inbound SET pending=0,core_id=?1,terminal_at=?2 WHERE id=?3",
                params![receipt.final_core_message_id, now, intent.job],
            )?;
            tx.execute(
                "UPDATE hub_core_handoff_health SET failures=0,first_failure_at=0,last_failure_at=0
                WHERE endpoint=?1 AND generation=?2",
                params![intent.endpoint, intent.generation],
            )?;
        }
        tx.commit()?;
        Ok((result, None))
    }
}

struct Handoff {
    recipient: String,
    body: String,
    external: String,
    pending: bool,
    lease: Option<String>,
    core_id: Option<String>,
    source: String,
    account: String,
    chat: i64,
    topic: i64,
}
