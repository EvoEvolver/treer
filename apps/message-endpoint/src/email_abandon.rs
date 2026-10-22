//! Local audited abandonment only; no owner authentication or HTTP route.
use super::{valid_time, Error, Queue};
use crate::{
    hub::{Directory, Hub},
    relay::Mailbox,
    store::valid_id,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

impl<M: Mailbox + Directory> Hub<M> {
    /// Caller must freshly authenticate a human owner and confirm the exact item.
    /// Only a paused current binding can abandon an intent never reserved for
    /// dispatch. Retain frozen bytes and a terminal tombstone; never infer that
    /// dispatching/uncertain mail was not sent, or release its reservation.
    pub fn abandon_prepared_email_audited_now(
        &mut self,
        endpoint: &str,
        revision: i64,
        intent_id: &str,
        actor: &str,
    ) -> Result<(), Error> {
        self.abandon_prepared_with_clock(endpoint, revision, intent_id, actor, || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|v| i64::try_from(v.as_secs()).ok())
                .ok_or(Error::Invalid)
        })
    }
    #[cfg(test)]
    fn abandon_prepared_email_audited(
        &mut self,
        endpoint: &str,
        revision: i64,
        intent_id: &str,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        self.abandon_prepared_with_clock(endpoint, revision, intent_id, actor, || Ok(now))
    }
    fn abandon_prepared_with_clock(
        &mut self,
        endpoint: &str,
        revision: i64,
        intent_id: &str,
        actor: &str,
        clock: impl FnOnce() -> Result<i64, Error>,
    ) -> Result<(), Error> {
        if !valid_id(endpoint) || !valid_id(intent_id) || revision < 1 || revision == i64::MAX {
            return Err(Error::Invalid);
        }
        if self.blocked().map_err(|_| Error::PrivateStorage)? {
            return Err(Error::Denied);
        }
        self.recovery_schema().map_err(|_| Error::PrivateStorage)?;
        Queue::new(&mut self.store)?;
        let tx = self
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = clock()?;
        if !valid_time(now) {
            return Err(Error::Invalid);
        }
        let detail = serde_json::to_string(&serde_json::json!({"item":intent_id,"previous_state":"prepared","new_revision":revision+1})).map_err(|_| Error::Invalid)?;
        let context = crate::owner_actions::AuditContext::with_detail(actor, now, &detail)
            .map_err(|_| Error::Invalid)?;
        // A retry of the same confirmed command is a no-op, including after
        // an unrelated revision change. Never use it to resurrect an old item.
        let already: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM email_intents i JOIN email_dispatch d ON d.id=i.id JOIN endpoints e ON e.id=d.endpoint AND e.generation=d.generation WHERE i.id=?1 AND d.endpoint=?2 AND i.endpoint=d.endpoint AND i.generation=d.generation AND i.state=2 AND i.mode='proxy_abandoned' AND d.state='prepared' AND EXISTS(SELECT 1 FROM hub_owner_audit a WHERE a.action='abandon_prepared_email' AND a.endpoint=?2 AND a.revision=?3 AND a.transfer=?4))",params![intent_id,endpoint,revision,detail],|r|r.get(0))?;
        if already {
            tx.commit()?;
            self.store
                .recheck_private_storage()
                .map_err(|_| Error::PrivateStorage)?;
            return Ok(());
        }
        let current: Option<(Option<String>,Option<i64>)> = tx.query_row(
            "SELECT i.nonce,i.lease_until FROM email_dispatch d JOIN email_intents i ON i.id=d.id
            JOIN endpoints e ON e.id=d.endpoint AND e.generation=d.generation
            JOIN hub_bindings b ON b.endpoint=e.id
            WHERE d.id=?1 AND d.endpoint=?2 AND d.state='prepared'
            AND i.endpoint=d.endpoint AND i.generation=d.generation
            AND i.state=0 AND i.mode IS NULL AND e.paused=1 AND b.revision=?3",
            params![intent_id, endpoint, revision],
            |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let (nonce, lease) = current.ok_or(Error::Conflict)?;
        crate::owner_actions::record(
            &tx,
            Some(&context),
            "abandon_prepared_email",
            endpoint,
            revision,
        )
        .map_err(|_| Error::PrivateStorage)?;
        if tx.execute("UPDATE email_intents SET state=2,mode='proxy_abandoned',nonce=NULL,lease_until=NULL WHERE id=?1 AND state=0 AND mode IS NULL AND nonce IS ?2 AND lease_until IS ?3",params![intent_id,nonce,lease])? != 1 {
            return Err(Error::Conflict);
        }
        if tx.execute(
            "UPDATE hub_bindings SET revision=revision+1 WHERE endpoint=?1 AND revision=?2",
            params![endpoint, revision],
        )? != 1
        {
            return Err(Error::Conflict);
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "email_abandon_tests.rs"]
mod tests;
