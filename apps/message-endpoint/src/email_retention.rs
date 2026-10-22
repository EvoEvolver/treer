//! Owner-triggered terminal-thread payload retirement. Dedupe identities survive.
use super::{Error, Queue};
use crate::{
    hub::{Directory, Hub},
    relay::Mailbox,
    store::valid_id,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
impl<M: Mailbox + Directory> Hub<M> {
    /// Fixed seven-day minimum age. At most twenty of EACH record class per call.
    /// Pending replies, uncertainty, active leases and incomplete notifications deny.
    pub fn purge_email_payload_now(
        &mut self,
        endpoint: &str,
        revision: i64,
        intent: &str,
        actor: &str,
    ) -> Result<(), Error> {
        if !valid_id(endpoint) || !valid_id(intent) || revision < 1 || revision == i64::MAX {
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
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| Error::Invalid)?
                .as_secs(),
        )
        .map_err(|_| Error::Invalid)?;
        let thread:Option<String>=tx.query_row("SELECT i.thread FROM email_intents i JOIN endpoints e ON e.id=i.endpoint JOIN hub_bindings b ON b.endpoint=e.id WHERE i.id=?1 AND i.endpoint=?2 AND e.paused=1 AND b.revision=?3",params![intent,endpoint,revision],|r|r.get(0)).optional()?;
        let thread = thread.ok_or(Error::Conflict)?;
        let unsafe_rows:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM email_intents i WHERE i.thread=?1 AND (i.state=0 OR i.nonce IS NOT NULL OR i.lease_until IS NOT NULL)) OR EXISTS(SELECT 1 FROM email_replies r JOIN inbound b ON b.id=r.job WHERE r.thread=?1 AND b.pending<>0) OR EXISTS(SELECT 1 FROM email_messages WHERE thread=?1 AND at>?2) OR EXISTS(SELECT 1 FROM email_replies WHERE thread=?1 AND at>?2) OR EXISTS(SELECT 1 FROM email_intents i LEFT JOIN email_terminal_retention t ON t.id=i.id WHERE i.thread=?1 AND (t.at IS NULL OR t.at>?2))",params![thread,now.saturating_sub(7*86400)],|r|r.get(0))?;
        if unsafe_rows {
            return Err(Error::Conflict);
        }
        let remaining: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM email_messages WHERE thread=?1 AND body<>'') OR EXISTS(SELECT 1 FROM email_replies WHERE thread=?1 AND body<>'') OR EXISTS(SELECT 1 FROM email_intents i WHERE i.thread=?1 AND NOT EXISTS(SELECT 1 FROM email_retired_intents r WHERE r.id=i.id))",[&thread],|r|r.get(0))?;
        if !remaining {
            return Err(Error::Conflict);
        }
        let detail=serde_json::json!({"item":intent,"decision":"terminal_payload_retired","minimum_age_days":7,"tombstones_retained":true}).to_string();
        let audit = crate::owner_actions::AuditContext::with_detail(actor, now, &detail)
            .map_err(|_| Error::Invalid)?;
        crate::owner_actions::record(&tx, Some(&audit), "purge_email_payload", endpoint, revision)
            .map_err(|_| Error::PrivateStorage)?;
        // Closing first prevents a new reply or context from adding work between batches.
        tx.execute(
            "INSERT OR IGNORE INTO email_purged_threads VALUES(?1,?2)",
            params![thread, now],
        )?;
        tx.execute("INSERT INTO email_outbox_purge SELECT id FROM email_intents i WHERE thread=?1 AND mode='outbox' AND NOT EXISTS(SELECT 1 FROM email_retired_intents r WHERE r.id=i.id) ORDER BY id LIMIT 20",[&thread])?;
        tx.execute("UPDATE email_dispatch SET payload=x'' WHERE id IN (SELECT id FROM email_intents i WHERE thread=?1 AND NOT EXISTS(SELECT 1 FROM email_retired_intents r WHERE r.id=i.id) ORDER BY id LIMIT 20)",[&thread])?;
        tx.execute("INSERT INTO email_retired_intents SELECT id FROM email_intents i WHERE thread=?1 AND NOT EXISTS(SELECT 1 FROM email_retired_intents r WHERE r.id=i.id) ORDER BY id LIMIT 20",[&thread])?;
        tx.execute("UPDATE email_messages SET body='' WHERE rowid IN (SELECT rowid FROM email_messages WHERE thread=?1 AND body<>'' ORDER BY rowid LIMIT 20)",[&thread])?;
        tx.execute("UPDATE inbound SET body='' WHERE id IN (SELECT job FROM email_replies WHERE thread=?1 AND body<>'' ORDER BY rowid LIMIT 20) AND pending=0",[&thread])?;
        tx.execute("UPDATE email_replies SET body='' WHERE rowid IN (SELECT rowid FROM email_replies WHERE thread=?1 AND body<>'' ORDER BY rowid LIMIT 20)",[&thread])?;
        tx.execute("UPDATE email_threads SET address='' WHERE id=?1", [&thread])?;
        tx.execute(
            "UPDATE hub_bindings SET revision=revision+1 WHERE endpoint=?1 AND revision=?2",
            params![endpoint, revision],
        )?;
        tx.commit()?;
        Ok(())
    }
}
impl Queue<'_> {
    /// Restart-safe file cleanup after durable retirement. No caller paths; the
    /// worker supplies its operator-configured private directory. Never create it.
    pub fn purge_retired_outbox(&mut self, directory: &std::path::Path) -> Result<usize, Error> {
        use rustix::fs::{self, AtFlags, Mode, OFlags};
        self.store
            .recheck_private_storage()
            .map_err(|_| Error::PrivateStorage)?;
        let dir = crate::private_store::open_directory(directory, false)
            .map_err(|_| Error::PrivateStorage)?;
        let mut stmt = self
            .store
            .db
            .prepare("SELECT id FROM email_outbox_purge ORDER BY id LIMIT 20")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for id in &ids {
            if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::Invalid);
            }
            let name = format!("{id}.json");
            match fs::openat(
                &dir,
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            ) {
                Ok(file) => {
                    crate::private_store::check_file(&file).map_err(|_| Error::PrivateStorage)?;
                    fs::unlinkat(&dir, name.as_str(), AtFlags::empty())
                        .map_err(|_| Error::PrivateStorage)?;
                    fs::fsync(&dir).map_err(|_| Error::PrivateStorage)?;
                }
                Err(rustix::io::Errno::NOENT) => {}
                Err(_) => return Err(Error::PrivateStorage),
            }
            fs::fsync(&dir).map_err(|_| Error::PrivateStorage)?;
            self.store
                .db
                .execute("DELETE FROM email_outbox_purge WHERE id=?1", [id])?;
        }
        self.store
            .recheck_private_storage()
            .map_err(|_| Error::PrivateStorage)?;
        Ok(ids.len())
    }
}
