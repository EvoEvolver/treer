//! Atomic queue completion boundary for negotiated receipt mode. Not dispatched
//! by Core workers until mixed-peer capability negotiation is implemented.
//! Core deliveries are obtained from this hub's authenticated workspace mailbox;
//! no HTTP body or client-supplied principal may construct this authority.
use crate::{
    core::Delivery,
    hub::{Directory, Error, Hub},
    protocol::{Channel, Envelope, ExternalSource, Fragment, SenderKind},
    relay::Mailbox,
    telegram::CoreIntent,
    telegram_receipts::{Expected, Receipt, Scope},
    telegram_reply_store::{record_in_transaction, HandoffState, Recorded},
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

impl<M: Mailbox + Directory> Hub<M> {
    /// The local correlation/lease comes from prepare_telegram_forward, never
    /// from receipt JSON. This method commits before returning permission to ack.
    /// Conflict is an error AFTER committing a bounded operator-visible pause;
    /// the original queue row remains pending and the Core delivery unacked.
    /// A persisted receipt-mode pin is required; legacy completion rejects it.
    pub fn complete_telegram_receipt(
        &mut self,
        delivery: &Delivery,
        intent: &CoreIntent,
        now: i64,
    ) -> Result<Recorded, Error> {
        if now < crate::transfers::TOMBSTONE_TTL
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
            job_id: job_id.clone(),
            recipient: row.recipient,
            body: row.body,
            context_ids: vec![],
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
        Ok(result)
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
