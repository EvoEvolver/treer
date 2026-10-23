//! Durable email mechanics for the explicitly configured outbox runtime.
//! Callers must supply freshly verified owner/member authority and Core readback;
//! these storage primitives are not authentication or an alternative owner API.
use crate::{
    protocol::{Envelope, SenderKind, MAX_BODY},
    store::{valid_id, Store},
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[path = "email_abandon.rs"]
mod abandon;
#[path = "email_retention.rs"]
mod retention;

#[path = "email_delivery.rs"]
pub mod delivery;
#[path = "email_proxy.rs"]
pub mod proxy;

#[cfg(test)]
pub const REAL_RECIPIENT: &str = "doomspec@outlook.com";
const MAX_MESSAGES: i64 = 10_000;
const MAX_BYTES: i64 = 128 * 1024 * 1024;
#[derive(Debug, Error)]
pub enum Error {
    #[error("email_storage_unavailable")]
    Storage(#[from] rusqlite::Error),
    #[error("email_private_storage_unavailable")]
    PrivateStorage,
    #[error("email_input_invalid")]
    Invalid,
    #[error("email_reply_unavailable")]
    Denied,
    #[error("email_idempotency_conflict")]
    Conflict,
    #[error("email_capacity_exceeded")]
    Full,
    #[error("email_rate_limited")]
    Rate,
    #[error("email_lease_unavailable")]
    Lease,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    pub address: String,
    pub member_id: String,
}
impl Contact {
    pub fn normalized(mut self) -> Result<Self, Error> {
        if !valid_id(&self.member_id)
            || self.member_id.len() > 128
            || !self.address.is_ascii()
            || self.address.len() > 254
        {
            return Err(Error::Invalid);
        }
        let (local, domain) = self.address.split_once('@').ok_or(Error::Invalid)?;
        if local.is_empty()
            || local.len() > 64
            || local.starts_with('.')
            || local.ends_with('.')
            || local.contains("..")
            || !local
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
            || !domain.contains('.')
            || domain.len() > 253
            || domain.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(Error::Invalid);
        }
        self.address.make_ascii_lowercase();
        Ok(self)
    }
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Outbox,
    Proxy,
}
#[derive(Clone, Serialize)]
pub struct Notification {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub idempotency_key: String,
}
/// Private lease capability: never serialized to browser status or audit.
pub struct Intent {
    id: String,
    nonce: String,
    thread: String,
    endpoint: String,
    generation: i64,
    address: String,
}
impl Intent {
    pub fn notification(&self, public_url: &str, _mode: Mode) -> Result<Notification, Error> {
        if public_url.len() > 2048 {
            return Err(Error::Invalid);
        }
        let mut url = reqwest::Url::parse(public_url).map_err(|_| Error::Invalid)?;
        let loopback = url
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
        url.set_fragment(Some(&format!("email-thread={}", self.thread)));
        Ok(Notification {
            to: self.address.clone(),
            subject: "Treer conversation update".into(),
            text: format!(
                "New Agent message in thread {}.\n\nRead and reply (Treer login required):\n{}",
                self.thread, url
            ),
            idempotency_key: self.id.clone(),
        })
    }
}
#[derive(Serialize)]
pub struct Thread {
    pub id: String,
    pub endpoint_id: String,
    pub recipient_agent_id: String,
    pub messages: Vec<Text>,
}
/// A member-scoped index contains no message bodies or contact addresses.
#[derive(Serialize)]
pub struct Inbox {
    pub threads: Vec<ThreadSummary>,
    pub truncated: bool,
}
#[derive(Serialize)]
pub struct ThreadSummary {
    pub id: String,
    pub endpoint_id: String,
    pub recipient_agent_id: String,
    pub updated_at: i64,
    pub message_count: i64,
}
#[derive(Serialize)]
pub struct Text {
    pub source_id: String,
    pub body: String,
    pub created_at: i64,
    pub member_id: Option<String>,
}
fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for p in parts {
        hash.update((p.len() as u64).to_be_bytes());
        hash.update(p.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
fn valid_time(now: i64) -> bool {
    (0..=i64::MAX - 172800).contains(&now)
}
fn nonce(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn capacity(
    tx: &Transaction<'_>,
    limits: crate::store::Limits,
    endpoint: &str,
    bytes: usize,
) -> Result<(), Error> {
    let (count, size): (i64, i64) = tx.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(body AS BLOB))),0) FROM (SELECT body FROM email_messages UNION ALL SELECT body FROM email_replies UNION ALL SELECT payload AS body FROM email_dispatch)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (local,local_size):(i64,i64)=tx.query_row("SELECT count(*),coalesce(sum(length(CAST(m.body AS BLOB))),0) FROM (SELECT thread,body FROM email_messages UNION ALL SELECT thread,body FROM email_replies UNION ALL SELECT i.thread,d.payload AS body FROM email_dispatch d JOIN email_intents i ON i.id=d.id) m JOIN email_threads t ON t.id=m.thread WHERE t.endpoint=?1",[endpoint],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if count >= limits.global_count.min(MAX_MESSAGES)
        || size.saturating_add(bytes as i64) > limits.global_bytes.min(MAX_BYTES)
        || local >= limits.endpoint_count.min(1000)
        || local_size.saturating_add(bytes as i64) > limits.endpoint_bytes.min(16 * 1024 * 1024)
    {
        return Err(Error::Full);
    }
    Ok(())
}
fn active(
    tx: &Transaction<'_>,
    endpoint: &str,
    generation: i64,
) -> Result<(String, Contact), Error> {
    tx.query_row("SELECT e.recipient,t.address,t.member FROM endpoints e JOIN email_targets t ON e.id=t.endpoint AND e.generation=t.generation WHERE e.id=?1 AND e.generation=?2 AND e.paused=0 AND t.revoked=0",params![endpoint,generation],|r|Ok((r.get(0)?,Contact{address:r.get(1)?,member_id:r.get(2)?}))).optional()?.ok_or(Error::Denied)
}
pub(crate) fn bound(store: &Store, endpoint: &str) -> Result<bool, Error> {
    let exists: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='email_targets')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(false);
    }
    Ok(store.db.query_row("SELECT EXISTS(SELECT 1 FROM email_targets t JOIN endpoints e ON e.id=t.endpoint AND e.generation=t.generation WHERE e.id=?1)",[endpoint],|r|r.get(0))?)
}
/// Current non-revoked target, including while paused. Missing legacy tables
/// mean no email binding, never a guessed target.
pub(crate) fn contact(store: &Store, endpoint: &str) -> Result<Option<Contact>, Error> {
    let exists: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='email_targets')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    Ok(store.db.query_row("SELECT t.address,t.member FROM email_targets t JOIN endpoints e ON e.id=t.endpoint AND e.generation=t.generation WHERE e.id=?1 AND t.revoked=0",[endpoint],|r|Ok(Contact{address:r.get(0)?,member_id:r.get(1)?})).optional()?)
}
impl<M: crate::relay::Mailbox + crate::hub::Directory> crate::hub::Hub<M> {
    /// Called by the email worker only after fresh membership and Core checks.
    /// The ordinary Core inbound lease and idempotency survive retries/restart.
    pub(crate) fn prepare_email_forward(
        &mut self,
        endpoint: &str,
        now: i64,
        token: &str,
    ) -> Result<Option<crate::telegram::CoreIntent>, Error> {
        if self.blocked().map_err(|_| Error::Denied)? {
            return Err(Error::Denied);
        }
        let view = self.active(endpoint, now).map_err(|_| Error::Denied)?;
        if contact(&self.store, endpoint)?.is_none() {
            return Err(Error::Denied);
        }
        let Some(job) = self
            .store
            .lease(endpoint, view.binding.generation, now, token)
            .map_err(|_| Error::Lease)?
        else {
            return Ok(None);
        };
        let (thread,reply,context):(String,String,String)=self.store.db.query_row("SELECT thread,id,context FROM email_replies WHERE job=?1 AND endpoint=?2 AND generation=?3",params![job.job_id,endpoint,view.binding.generation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let envelope = Envelope::Deliver {
            receipt_scope: None,
            job_id: job.idempotency_key,
            recipient: job.recipient,
            body: job.body,
            context_ids: serde_json::from_str(&context).map_err(|_| Error::Invalid)?,
            source: crate::protocol::ExternalSource {
                channel: crate::protocol::Channel::Email,
                account_id: endpoint.into(),
                conversation_id: thread,
                message_id: reply,
            },
        };
        let parts = envelope
            .fragments(endpoint, view.binding.generation as u64)
            .map_err(|_| Error::Invalid)?
            .into_iter()
            .map(|body| {
                let frame = crate::protocol::Fragment::parse(&body).map_err(|_| Error::Invalid)?;
                Ok((frame.idempotency_key(), body))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Some(crate::telegram::CoreIntent {
            endpoint: endpoint.into(),
            generation: view.binding.generation,
            job: job.job_id,
            token: token.into(),
            parts,
        }))
    }
    /// Owner executor has checked current membership without holding hub state.
    /// Commit immutable target, revision and audit together; remain paused.
    pub(crate) fn bind_email_audited(
        &mut self,
        endpoint: &str,
        revision: i64,
        contact: Contact,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_not_terminal_held(endpoint)
            .map_err(|_| Error::Denied)?;
        let contact = contact.normalized()?;
        let view = self.view(endpoint).map_err(|_| Error::Denied)?;
        if !view.paused || view.revision != revision {
            return Err(Error::Conflict);
        }
        self.store
            .recheck_private_storage()
            .map_err(|_| Error::PrivateStorage)?;
        self.recovery_schema().map_err(|_| Error::PrivateStorage)?;
        self.telegram_schema().map_err(|_| Error::PrivateStorage)?;
        Queue::new(&mut self.store)?;
        let context =
            crate::owner_actions::AuditContext::new(actor, now).map_err(|_| Error::Invalid)?;
        let tx = self.store.db.transaction()?;
        let telegram: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM telegram_targets WHERE endpoint=?1)",
            [endpoint],
            |r| r.get(0),
        )?;
        if telegram {
            return Err(Error::Conflict);
        }
        crate::owner_actions::record(&tx, Some(&context), "bind_email", endpoint, revision)
            .map_err(|_| Error::PrivateStorage)?;
        tx.execute(
            "INSERT INTO email_targets(endpoint,generation,address,member) VALUES(?1,?2,?3,?4)",
            params![
                endpoint,
                view.binding.generation,
                contact.address,
                contact.member_id
            ],
        )?;
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
/// Shared private SQLite, with additive tables only. This does not start workers.
pub struct Queue<'a> {
    store: &'a mut Store,
}
impl<'a> Queue<'a> {
    pub fn new(store: &'a mut Store) -> Result<Self, Error> {
        store.db.execute_batch("CREATE TABLE IF NOT EXISTS email_targets(endpoint TEXT NOT NULL REFERENCES endpoints(id),generation INTEGER NOT NULL,address TEXT NOT NULL,member TEXT NOT NULL,revoked INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(endpoint,generation));
        CREATE TABLE IF NOT EXISTS email_threads(id TEXT PRIMARY KEY,endpoint TEXT NOT NULL REFERENCES endpoints(id),generation INTEGER NOT NULL,recipient TEXT NOT NULL,address TEXT NOT NULL,member TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS email_messages(thread TEXT NOT NULL REFERENCES email_threads(id),source TEXT NOT NULL,body TEXT NOT NULL,digest TEXT NOT NULL,at INTEGER NOT NULL,PRIMARY KEY(thread,source));
        CREATE TABLE IF NOT EXISTS email_intents(id TEXT PRIMARY KEY,thread TEXT NOT NULL REFERENCES email_threads(id),endpoint TEXT NOT NULL,generation INTEGER NOT NULL,state INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,next_try INTEGER NOT NULL DEFAULT 0,nonce TEXT,lease_until INTEGER,mode TEXT);
        CREATE TABLE IF NOT EXISTS email_replies(id TEXT PRIMARY KEY,thread TEXT NOT NULL REFERENCES email_threads(id),endpoint TEXT NOT NULL,generation INTEGER NOT NULL,member TEXT NOT NULL,body TEXT NOT NULL,digest TEXT NOT NULL,job INTEGER NOT NULL REFERENCES inbound(id),at INTEGER NOT NULL,context TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS email_purged_threads(thread TEXT PRIMARY KEY REFERENCES email_threads(id),at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS email_retired_intents(id TEXT PRIMARY KEY REFERENCES email_intents(id));
        CREATE TABLE IF NOT EXISTS email_terminal_retention(id TEXT PRIMARY KEY REFERENCES email_intents(id),at INTEGER NOT NULL);
        INSERT OR IGNORE INTO email_terminal_retention SELECT id,CAST(strftime('%s','now') AS INTEGER) FROM email_intents WHERE state<>0;
        CREATE TABLE IF NOT EXISTS email_outbox_purge(id TEXT PRIMARY KEY REFERENCES email_intents(id));
        CREATE TABLE IF NOT EXISTS email_attempts(id INTEGER PRIMARY KEY,endpoint TEXT NOT NULL,at INTEGER NOT NULL);")?;
        delivery::schema(&store.db)?;
        Ok(Self { store })
    }
    /// Owner executor must check current membership and atomically audit binding
    /// before exposing this through any request handler. No automatic activation.
    pub fn bind_paused(
        &mut self,
        endpoint: &str,
        generation: i64,
        contact: Contact,
    ) -> Result<(), Error> {
        let contact = contact.normalized()?;
        let tx = self.store.db.transaction()?;
        let paused: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM endpoints WHERE id=?1 AND generation=?2 AND paused=1)",
            params![endpoint, generation],
            |r| r.get(0),
        )?;
        if !paused {
            return Err(Error::Denied);
        }
        tx.execute(
            "INSERT INTO email_targets(endpoint,generation,address,member) VALUES(?1,?2,?3,?4)",
            params![endpoint, generation, contact.address, contact.member_id],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// The owner revoke primitive is containment: pending leases cannot be newly
    /// dispatched afterwards. An already-issued network request is not recalled.
    pub fn revoke(&mut self, endpoint: &str) -> Result<(), Error> {
        self.store.db.execute(
            "UPDATE email_targets SET revoked=1 WHERE endpoint=?1",
            [endpoint],
        )?;
        Ok(())
    }
    /// Durable receipt state for a reassembled Core message; completion is
    /// recorded only after the selected transport has accepted its intent.
    pub(crate) fn receipt(
        &self,
        endpoint: &str,
        generation: i64,
        message: &str,
    ) -> Result<Option<i64>, Error> {
        Ok(self.store.db.query_row("SELECT i.state FROM email_intents i JOIN email_messages m ON m.thread=i.thread WHERE i.endpoint=?1 AND i.generation=?2 AND m.source=?3 AND i.id=?4",params![endpoint,generation,message,digest(&["email-notify",endpoint,&generation.to_string(),message])],|r|r.get(0)).optional()?)
    }
    /// Only a reassembled authenticated endpoint envelope, after current Core
    /// readback/member admission. Body is stored privately; mail never uses it.
    pub fn admit(
        &mut self,
        endpoint: &str,
        generation: i64,
        envelope: &Envelope,
        now: i64,
    ) -> Result<String, Error> {
        let Envelope::Outbound {
            message_id,
            original_sender,
            body,
            context_ids,
            ..
        } = envelope
        else {
            return Err(Error::Invalid);
        };
        if !valid_time(now)
            || !valid_id(message_id)
            || body.len() > MAX_BODY
            || context_ids.len() > 64
            || context_ids.iter().any(|id| !valid_id(id))
        {
            return Err(Error::Invalid);
        }
        let tx = self.store.db.transaction()?;
        let (recipient, contact) = active(&tx, endpoint, generation)?;
        if original_sender.kind != SenderKind::Agent || original_sender.id != recipient {
            return Err(Error::Denied);
        }
        let mut thread = None;
        for context in context_ids {
            thread=tx.query_row("SELECT t.id FROM email_threads t JOIN email_messages m ON m.thread=t.id WHERE t.endpoint=?1 AND t.generation=?2 AND m.source=?3",params![endpoint,generation,context],|r|r.get::<_,String>(0)).optional()?;
            if thread.is_none() {
                thread=tx.query_row("SELECT r.thread FROM email_replies r JOIN inbound i ON i.id=r.job WHERE r.endpoint=?1 AND r.generation=?2 AND i.core_id=?3",params![endpoint,generation,context],|r|r.get::<_,String>(0)).optional()?;
            }
            if thread.is_some() {
                break;
            }
        }
        let thread = thread.unwrap_or_else(|| {
            digest(&[
                "email-thread",
                endpoint,
                &generation.to_string(),
                message_id,
            ])
        });
        let prior:Option<(String,String)>=tx.query_row("SELECT t.id,m.digest FROM email_threads t JOIN email_messages m ON m.thread=t.id WHERE t.endpoint=?1 AND t.generation=?2 AND m.source=?3",params![endpoint,generation,message_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let mut fingerprint = vec![body.as_str()];
        fingerprint.extend(context_ids.iter().map(String::as_str));
        let hash = digest(&fingerprint);
        if let Some((old, old_hash)) = prior {
            if old_hash != hash {
                return Err(Error::Conflict);
            }
            return Ok(old);
        }
        let retired: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM email_purged_threads WHERE thread=?1)",
            [&thread],
            |r| r.get(0),
        )?;
        if retired {
            return Err(Error::Denied);
        }
        capacity(&tx, self.store.limits, endpoint, body.len())?;
        tx.execute(
            "INSERT OR IGNORE INTO email_threads VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                thread,
                endpoint,
                generation,
                recipient,
                contact.address,
                contact.member_id
            ],
        )?;
        tx.execute(
            "INSERT INTO email_messages VALUES(?1,?2,?3,?4,?5)",
            params![thread, message_id, body, hash, now],
        )?;
        let id = digest(&[
            "email-notify",
            endpoint,
            &generation.to_string(),
            message_id,
        ]);
        tx.execute(
            "INSERT INTO email_intents(id,thread,endpoint,generation) VALUES(?1,?2,?3,?4)",
            params![id, thread, endpoint, generation],
        )?;
        tx.commit()?;
        Ok(thread)
    }
    /// Fresh member authentication is required BEFORE calling; an owner does
    /// not receive implicit read access to another member's private threads.
    pub fn thread(
        &self,
        id: &str,
        member: &crate::owner_auth::Member,
        now: i64,
    ) -> Result<Thread, Error> {
        if !valid_time(now) || member.expires_at() <= now {
            return Err(Error::Denied);
        }
        self.thread_for(id, member.id())
    }
    /// Uses the same live binding/member scope as thread reads. Pausing preserves
    /// readable history; revocation or a generation/contact change removes it.
    pub fn inbox(&self, member: &crate::owner_auth::Member, now: i64) -> Result<Inbox, Error> {
        if !valid_time(now) || member.expires_at() <= now {
            return Err(Error::Denied);
        }
        let mut query = self.store.db.prepare(
            "SELECT t.id,t.endpoint,t.recipient,max(m.at),count(*)
             FROM email_threads t
             JOIN endpoints e ON e.id=t.endpoint AND e.generation=t.generation
             JOIN email_targets c ON c.endpoint=t.endpoint AND c.generation=t.generation
               AND c.address=t.address AND c.member=t.member AND c.revoked=0
             JOIN (SELECT thread,at FROM email_messages UNION ALL SELECT thread,at FROM email_replies) m ON m.thread=t.id
             WHERE t.member=?1 GROUP BY t.id
             ORDER BY max(m.at) DESC,t.id DESC LIMIT 51",
        )?;
        let mut threads = query
            .query_map([member.id()], |r| {
                Ok(ThreadSummary {
                    id: r.get(0)?,
                    endpoint_id: r.get(1)?,
                    recipient_agent_id: r.get(2)?,
                    updated_at: r.get(3)?,
                    message_count: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = threads.len() > 50;
        threads.truncate(50);
        Ok(Inbox { threads, truncated })
    }
    fn thread_for(&self, id: &str, verified_member: &str) -> Result<Thread, Error> {
        let retired: bool = self.store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM email_purged_threads WHERE thread=?1)",
            [id],
            |r| r.get(0),
        )?;
        if retired {
            return Err(Error::Denied);
        }
        let (endpoint,generation,recipient,address,member):(String,i64,String,String,String)=self.store.db.query_row("SELECT endpoint,generation,recipient,address,member FROM email_threads WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(Error::Denied)?;
        let current:bool=self.store.db.query_row("SELECT EXISTS(SELECT 1 FROM email_targets t JOIN endpoints e ON e.id=t.endpoint AND e.generation=t.generation WHERE t.endpoint=?1 AND t.generation=?2 AND t.address=?3 AND t.member=?4 AND t.revoked=0)",params![endpoint,generation,address,member],|r|r.get(0))?;
        if !current || member != verified_member {
            return Err(Error::Denied);
        }
        let mut query = self.store.db.prepare(
            "SELECT source,body,at,member FROM (SELECT source,body,at,NULL AS member FROM email_messages WHERE thread=?1 UNION ALL SELECT id AS source,body,at,member FROM email_replies WHERE thread=?1) ORDER BY at DESC,source DESC LIMIT 50",
        )?;
        let mut messages = query
            .query_map([id], |r| {
                Ok(Text {
                    source_id: r.get(0)?,
                    body: r.get(1)?,
                    created_at: r.get(2)?,
                    member_id: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(Thread {
            id: id.into(),
            endpoint_id: endpoint,
            recipient_agent_id: recipient,
            messages,
        })
    }
    /// Router must freshly authenticate Member before each call and enforce
    /// Origin/CSRF. Recipient and source identity are never browser parameters.
    pub fn reply(
        &mut self,
        thread: &str,
        member: &crate::owner_auth::Member,
        key: &str,
        body: &str,
        now: i64,
    ) -> Result<i64, Error> {
        if !valid_time(now) || member.expires_at() <= now {
            return Err(Error::Denied);
        }
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || body.trim().is_empty()
            || body.len() > 8192
        {
            return Err(Error::Invalid);
        }
        let view = self.thread_for(thread, member.id())?;
        let tx = self.store.db.transaction()?;
        let generation: i64 = tx.query_row(
            "SELECT generation FROM email_threads WHERE id=?1",
            [thread],
            |r| r.get(0),
        )?;
        let (recipient, contact) = active(&tx, &view.endpoint_id, generation)?;
        if contact.member_id != member.id() || recipient != view.recipient_agent_id {
            return Err(Error::Denied);
        }
        let id = digest(&["email-reply", thread, member.id(), key]);
        let hash = digest(&[body]);
        let prior: Option<(i64, String)> = tx
            .query_row(
                "SELECT job,digest FROM email_replies WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((job, previous)) = prior {
            return if hash == previous {
                Ok(job)
            } else {
                Err(Error::Conflict)
            };
        }
        capacity(&tx, self.store.limits, &view.endpoint_id, body.len())?;
        for (seconds, local_cap, global_cap) in [(60, 10, 60), (86400, 100, 1000)] {
            let (local, global): (i64, i64) = tx.query_row(
                "SELECT coalesce(sum(endpoint=?1),0),count(*) FROM email_replies WHERE at>?2",
                params![view.endpoint_id, now.saturating_sub(seconds)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if local >= local_cap || global >= global_cap {
                return Err(Error::Rate);
            }
        }
        let framed = reply_text(member.id(), body)?;
        let external = format!("email-{id}");
        let job = crate::store::enqueue_in(
            &tx,
            self.store.limits,
            crate::store::Inbound {
                endpoint: &view.endpoint_id,
                generation,
                external_id: &external,
                recipient: &recipient,
                body: &framed,
            },
        )
        .map_err(|error| match error {
            crate::store::Error::Full => Error::Full,
            _ => Error::Denied,
        })?;
        let context: String = tx.query_row(
            "SELECT source FROM email_messages WHERE thread=?1 ORDER BY at DESC,rowid DESC LIMIT 1",
            [thread],
            |r| r.get(0),
        )?;
        let contexts = serde_json::to_string(&vec![context]).map_err(|_| Error::Invalid)?;
        tx.execute(
            "INSERT INTO email_replies VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                id,
                thread,
                view.endpoint_id,
                generation,
                member.id(),
                body,
                hash,
                job,
                now,
                contexts
            ],
        )?;
        tx.commit()?;
        Ok(job)
    }
    /// Caller supplies CSPRNG nonce; limits count attempts, not successful mail.
    /// Do not hold the store mutex while sending the returned frozen intent.
    pub fn claim(
        &mut self,
        endpoint: &str,
        generation: i64,
        now: i64,
        lease_nonce: &str,
    ) -> Result<Option<Intent>, Error> {
        self.claim_for(endpoint, generation, now, lease_nonce, Mode::Outbox)
    }
    /// Future Proxy worker only. Prepared journal rows may be reclaimed with
    /// the same frozen bytes; ordinary outbox claims cannot see them.
    pub fn claim_proxy(
        &mut self,
        endpoint: &str,
        generation: i64,
        now: i64,
        lease_nonce: &str,
    ) -> Result<Option<Intent>, Error> {
        self.claim_for(endpoint, generation, now, lease_nonce, Mode::Proxy)
    }
    fn claim_for(
        &mut self,
        endpoint: &str,
        generation: i64,
        now: i64,
        lease_nonce: &str,
        mode: Mode,
    ) -> Result<Option<Intent>, Error> {
        if !valid_time(now) || !nonce(lease_nonce) {
            return Err(Error::Invalid);
        }
        let tx = self.store.db.transaction()?;
        let (_, contact) = active(&tx, endpoint, generation)?;
        tx.execute(
            "DELETE FROM email_attempts WHERE at<?1",
            [now.saturating_sub(172800)],
        )?;
        tx.execute("UPDATE email_intents SET state=2,nonce=NULL,lease_until=NULL WHERE endpoint=?1 AND generation=?2 AND state=0 AND attempts>=8 AND (lease_until IS NULL OR lease_until<=?3) AND NOT EXISTS(SELECT 1 FROM email_dispatch d WHERE d.id=email_intents.id AND (?4=0 OR d.state!='prepared'))",params![endpoint,generation,now,mode==Mode::Proxy])?;
        let candidate:Option<(String,String)>=tx.query_row("SELECT id,thread FROM email_intents WHERE endpoint=?1 AND generation=?2 AND state=0 AND attempts<8 AND next_try<=?3 AND (lease_until IS NULL OR lease_until<=?3) AND NOT EXISTS(SELECT 1 FROM email_dispatch d WHERE d.id=email_intents.id AND (?4=0 OR d.state!='prepared')) ORDER BY rowid LIMIT 1",params![endpoint,generation,now,mode==Mode::Proxy],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((id, thread)) = candidate else {
            tx.commit()?;
            return Ok(None);
        };
        for (seconds, local_cap, global_cap) in [(60, 10, 60), (86400, 100, 1000)] {
            let (local, global): (i64, i64) = tx.query_row(
                "SELECT coalesce(sum(endpoint=?1),0),count(*) FROM email_attempts WHERE at>?2",
                params![endpoint, now.saturating_sub(seconds)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if local >= local_cap || global >= global_cap {
                return Err(Error::Rate);
            }
        }
        tx.execute(
            "INSERT INTO email_attempts(endpoint,at) VALUES(?1,?2)",
            params![endpoint, now],
        )?;
        tx.execute("UPDATE email_intents SET attempts=attempts+1,nonce=?2,lease_until=?3,next_try=?3 WHERE id=?1",params![id,lease_nonce,now.saturating_add(60)])?;
        tx.commit()?;
        Ok(Some(Intent {
            id,
            nonce: lease_nonce.into(),
            thread,
            endpoint: endpoint.into(),
            generation,
            address: contact.address,
        }))
    }
    /// Fence against pause/revoke immediately before dispatch. Current directory
    /// membership and Hub Core readback are separate caller checks, never cached.
    pub fn dispatchable(&self, intent: &Intent, now: i64) -> Result<bool, Error> {
        let current:bool=self.store.db.query_row("SELECT EXISTS(SELECT 1 FROM email_intents i JOIN endpoints e ON e.id=i.endpoint AND e.generation=i.generation JOIN email_targets t ON t.endpoint=e.id AND t.generation=e.generation WHERE i.id=?1 AND i.nonce=?2 AND i.state=0 AND i.lease_until>?3 AND e.paused=0 AND t.revoked=0 AND t.address=?4 AND NOT EXISTS(SELECT 1 FROM email_dispatch d WHERE d.id=i.id))",params![intent.id,intent.nonce,now,intent.address],|r|r.get(0))?;
        Ok(current)
    }
    /// Persist acceptance even if owner paused after the send. Never turn a
    /// possible external success into a retry merely because pause raced it.
    pub fn complete(&mut self, intent: &Intent, mode: Mode) -> Result<(), Error> {
        if delivery::exists(&self.store.db, &intent.id)? {
            return Err(Error::Denied);
        }
        let changed=self.store.db.execute("UPDATE email_intents SET state=1,mode=?3,nonce=NULL,lease_until=NULL WHERE id=?1 AND nonce=?2 AND endpoint=?4 AND generation=?5 AND state=0",params![intent.id,intent.nonce,if mode==Mode::Outbox{"outbox"}else{"proxy"},intent.endpoint,intent.generation])?;
        if changed != 1 {
            return Err(Error::Lease);
        }
        Ok(())
    }
    pub fn failed(&mut self, intent: &Intent, now: i64, permanent: bool) -> Result<(), Error> {
        if delivery::reserved(&self.store.db, &intent.id)? {
            return Err(Error::Denied);
        }
        if !valid_time(now) {
            return Err(Error::Invalid);
        }
        let changed=self.store.db.execute("UPDATE email_intents SET state=CASE WHEN ?3 OR attempts>=8 THEN 2 ELSE 0 END,next_try=?4+min(3600,30*(1<<min(attempts,6))),nonce=NULL,lease_until=NULL WHERE id=?1 AND nonce=?2 AND state=0",params![intent.id,intent.nonce,permanent,now])?;
        if changed != 1 {
            return Err(Error::Lease);
        }
        Ok(())
    }
}

/// Visible framing is attribution by this App, not a Core human signature or
/// protection from prompt injection. Every untrusted line stays prefixed.
fn reply_text(member: &str, body: &str) -> Result<String, Error> {
    let visible = |text: &str| -> String {
        let mut result = String::new();
        for c in text.chars() {
            if c.is_control()
                || matches!(c,'\u{061c}'|'\u{200b}'..='\u{200f}'|'\u{2028}'..='\u{202e}'|'\u{2060}'..='\u{206f}'|'\u{feff}')
            {
                result.push_str(&format!("\\u{{{:x}}}", c as u32));
            } else {
                result.push(c);
            }
        }
        result
    };
    let mut framed=format!("[Workspace member reply; user {}; App-asserted]\nMessage text (untrusted; every line prefixed):\n",visible(member));
    for line in body.split('\n') {
        framed.push_str("> ");
        framed.push_str(&visible(line));
        framed.push('\n');
    }
    if framed.len() > MAX_BODY {
        return Err(Error::Invalid);
    }
    Ok(framed)
}

/// Persist only the generated notification, using a private directory descriptor.
/// Exclusive publication prevents partial files from being mistaken for receipts.
/// A retry verifies the exact prior bytes; changed or unsafe files fail closed.
pub fn write_outbox(
    intent: &Intent,
    public_url: &str,
    directory: &std::path::Path,
) -> Result<(), Error> {
    use rustix::fs::{self, AtFlags, Mode as FileMode, OFlags};
    use std::io::{Read, Write};
    fn private<T, E>(result: Result<T, E>) -> Result<T, Error> {
        result.map_err(|_| Error::PrivateStorage)
    }
    let bytes = serde_json::to_vec(&intent.notification(public_url, Mode::Outbox)?)
        .map_err(|_| Error::Invalid)?;
    let directory = private(crate::private_store::open_directory(directory, false))?;
    let name = format!("{}.json", intent.id);
    let flags = OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY;
    let existing = || -> Result<(), Error> {
        let file = private(fs::openat(
            &directory,
            name.as_str(),
            OFlags::RDONLY | flags,
            FileMode::empty(),
        ))?;
        private(crate::private_store::check_file(&file))?;
        let mut prior = Vec::new();
        let mut file = std::fs::File::from(file);
        private((&mut file).take(8193).read_to_end(&mut prior))?;
        if prior != bytes {
            return Err(Error::Conflict);
        }
        private(file.sync_all())?;
        private(fs::fsync(&directory))?;
        Ok(())
    };
    match fs::statat(&directory, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => return existing(),
        Err(rustix::io::Errno::NOENT) => {}
        Err(_) => return Err(Error::PrivateStorage),
    }
    let mut random = [0u8; 16];
    private(getrandom::fill(&mut random))?;
    let temporary = format!(
        ".email-{}.tmp",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let file = private(fs::openat(
        &directory,
        temporary.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | flags,
        FileMode::from_raw_mode(0o600),
    ))?;
    let result = (|| {
        private(crate::private_store::check_file(&file))?;
        let mut file = std::fs::File::from(file);
        private(file.write_all(&bytes))?;
        private(file.sync_all())?;
        match fs::linkat(
            &directory,
            temporary.as_str(),
            &directory,
            name.as_str(),
            AtFlags::empty(),
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {
                return existing();
            }
            Err(_) => return Err(Error::PrivateStorage),
        }
        Ok(())
    })();
    let cleanup = fs::unlinkat(&directory, temporary.as_str(), AtFlags::empty());
    result?;
    private(cleanup)?;
    private(fs::fsync(&directory))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{protocol::Sender, store::Limits};
    fn member(id: &str) -> crate::owner_auth::Member {
        crate::owner_auth::Member::synthetic(id, 200000)
    }
    fn fixture() -> Store {
        let mut store = Store::memory(Limits::default()).unwrap();
        store.register_paused("ag_email", 1, "ag_internal").unwrap();
        Queue::new(&mut store)
            .unwrap()
            .bind_paused(
                "ag_email",
                1,
                Contact {
                    address: "PERSON@example.test".into(),
                    member_id: "user_alice".into(),
                },
            )
            .unwrap();
        // Storage-only fixture; production activation uses the audited owner path.
        store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        store
    }
    #[test]
    fn email_snapshot_truncates_targets_without_breaking_settings() {
        let mut store = fixture();
        for n in 0..20 {
            let id = format!("ag_extra_{n}");
            // Simulate oversized imported/history state beyond the current 20-endpoint admission cap.
            store
                .db
                .execute(
                    "INSERT INTO endpoints(id,generation,recipient) VALUES (?1,1,'ag_internal')",
                    [&id],
                )
                .unwrap();
            Queue::new(&mut store)
                .unwrap()
                .bind_paused(
                    &id,
                    1,
                    Contact {
                        address: "person@example.test".into(),
                        member_id: "user_alice".into(),
                    },
                )
                .unwrap();
        }
        let snapshot = crate::email_workers::Snapshot::read(&store, None, Mode::Outbox).unwrap();
        assert_eq!(snapshot.targets.len(), 20);
        assert!(snapshot.targets_truncated);
        store
            .db
            .execute("DELETE FROM email_targets WHERE endpoint='ag_email'", [])
            .unwrap();
        let snapshot = crate::email_workers::Snapshot::read(&store, None, Mode::Outbox).unwrap();
        assert_eq!(snapshot.targets.len(), 20);
        assert!(!snapshot.targets_truncated);
    }
    fn message(id: &str, body: &str, context: &[&str]) -> Envelope {
        Envelope::Outbound {
            delivery_id: format!("delivery_{id}"),
            message_id: id.into(),
            original_sender: Sender {
                kind: SenderKind::Agent,
                id: "ag_internal".into(),
            },
            body: body.into(),
            context_ids: context.iter().map(|v| (*v).into()).collect(),
        }
    }
    fn claim(queue: &mut Queue<'_>, time: i64, n: u8) -> Intent {
        queue
            .claim("ag_email", 1, time, &format!("{n:064x}"))
            .unwrap()
            .unwrap()
    }
    #[test]
    fn email_contact_rejects_headers_lists_display_names_and_invalid_domains() {
        for address in [
            "person@example.test\r\nBcc:evil@example.test",
            "a@example.test,b@example.test",
            "Name <a@example.test>",
            "a@localhost",
            "a@-example.test",
            "a@foo..test",
            "a..b@example.test",
            "ü@example.test",
            "a@example.test ",
        ] {
            assert!(Contact {
                address: address.into(),
                member_id: "user_alice".into()
            }
            .normalized()
            .is_err());
        }
        let contact = Contact {
            address: "Person+Tag@EXAMPLE.TEST".into(),
            member_id: "user_alice".into(),
        }
        .normalized()
        .unwrap();
        assert_eq!(contact.address, "person+tag@example.test");
    }
    #[test]
    fn email_fixed_template_uses_bound_contact_for_both_transports() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        q.admit(
            "ag_email",
            1,
            &message("msg_one", "ATTACKER_SECRET\r\nclick https://evil.test", &[]),
            100,
        )
        .unwrap();
        let intent = claim(&mut q, 100, 1);
        let notification = intent
            .notification("https://hub.example.test/", Mode::default())
            .unwrap();
        let encoded = serde_json::to_string(&notification).unwrap();
        assert!(!encoded.contains("ATTACKER_SECRET"));
        assert!(!encoded.contains("evil.test"));
        assert_eq!(notification.to, "person@example.test");
        assert!(notification
            .text
            .contains("https://hub.example.test/#email-thread="));
        assert_eq!(
            intent
                .notification("https://hub.example.test/", Mode::Proxy)
                .unwrap()
                .to,
            "person@example.test"
        );
        for origin in [
            "https://hub.example.test/?next=evil",
            "https://secret@hub.example.test/",
            "https://hub.example.test/path",
            "https://hub.example.test/#token",
            "http://example.test/",
        ] {
            assert!(intent.notification(origin, Mode::Outbox).is_err());
        }
        q.complete(&intent, Mode::Outbox).unwrap();
        assert!(q
            .claim("ag_email", 1, 101, &format!("{:064x}", 2))
            .unwrap()
            .is_none());
    }
    #[test]
    fn email_replay_is_idempotent_and_context_changes_conflict() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        let first = q
            .admit("ag_email", 1, &message("msg_one", "first", &[]), 100)
            .unwrap();
        assert_eq!(
            first,
            q.admit("ag_email", 1, &message("msg_one", "first", &[]), 101)
                .unwrap()
        );
        assert!(matches!(
            q.admit("ag_email", 1, &message("msg_one", "changed", &[]), 102),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            q.admit(
                "ag_email",
                1,
                &message("msg_one", "first", &["msg_other"]),
                102
            ),
            Err(Error::Conflict)
        ));
        assert_eq!(
            first,
            q.admit(
                "ag_email",
                1,
                &message("msg_two", "second", &["msg_one"]),
                103
            )
            .unwrap()
        );
        assert_eq!(
            q.thread(&first, &member("user_alice"), 100)
                .unwrap()
                .messages
                .len(),
            2
        );
    }
    #[test]
    fn email_only_fixed_initiator_and_generation_may_create_threads() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        let mut envelope = message("msg_one", "body", &[]);
        if let Envelope::Outbound {
            original_sender, ..
        } = &mut envelope
        {
            original_sender.id = "ag_other".into();
        }
        assert!(matches!(
            q.admit("ag_email", 1, &envelope, 100),
            Err(Error::Denied)
        ));
        assert!(matches!(
            q.admit("ag_email", 2, &message("msg_one", "body", &[]), 100),
            Err(Error::Denied)
        ));
        q.store.pause("ag_email").unwrap();
        assert!(matches!(
            q.admit("ag_email", 1, &message("msg_one", "body", &[]), 100),
            Err(Error::Denied)
        ));
    }
    #[test]
    fn email_member_inbox_is_bounded_current_and_body_free() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        let mut last = String::new();
        for n in 0..51 {
            last = q
                .admit(
                    "ag_email",
                    1,
                    &message(&format!("msg_{n}"), "PRIVATE BODY", &[]),
                    100 + n,
                )
                .unwrap();
        }
        let inbox = q.inbox(&member("user_alice"), 200).unwrap();
        assert!(inbox.truncated);
        assert_eq!(inbox.threads.len(), 50);
        assert_eq!(inbox.threads[0].id, last);
        assert_eq!(inbox.threads[0].message_count, 1);
        let text = serde_json::to_string(&inbox).unwrap();
        assert!(!text.contains("PRIVATE BODY"));
        assert!(!text.contains("example.test"));
        assert!(q
            .inbox(&member("user_bob"), 200)
            .unwrap()
            .threads
            .is_empty());
        assert!(q
            .inbox(&member("user_owner"), 200)
            .unwrap()
            .threads
            .is_empty());
        assert!(q
            .inbox(
                &crate::owner_auth::Member::synthetic("user_alice", 200),
                200
            )
            .is_err());
        q.store
            .db
            .execute("UPDATE endpoints SET paused=1", [])
            .unwrap();
        assert_eq!(
            q.inbox(&member("user_alice"), 200).unwrap().threads.len(),
            50
        );
        q.store
            .db
            .execute("UPDATE email_targets SET revoked=1", [])
            .unwrap();
        assert!(q
            .inbox(&member("user_alice"), 200)
            .unwrap()
            .threads
            .is_empty());
        q.store
            .db
            .execute("UPDATE email_targets SET revoked=0,member='user_bob'", [])
            .unwrap();
        assert!(q
            .inbox(&member("user_alice"), 200)
            .unwrap()
            .threads
            .is_empty());
        assert!(q
            .inbox(&member("user_bob"), 200)
            .unwrap()
            .threads
            .is_empty());
        q.store
            .db
            .execute("UPDATE email_targets SET member='user_alice'", [])
            .unwrap();
        q.store
            .db
            .execute("UPDATE endpoints SET generation=2", [])
            .unwrap();
        assert!(q
            .inbox(&member("user_alice"), 200)
            .unwrap()
            .threads
            .is_empty());
    }

    #[test]
    fn email_thread_id_is_not_authority_and_revocation_blocks_read_and_dispatch() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        let thread = q
            .admit("ag_email", 1, &message("msg_one", "private", &[]), 100)
            .unwrap();
        let intent = claim(&mut q, 100, 1);
        assert!(q.thread(&thread, &member("user_alice"), 100).is_ok());
        assert!(matches!(
            q.thread(&thread, &member("user_bob"), 100),
            Err(Error::Denied)
        ));
        assert!(matches!(
            q.thread(&thread, &member("user_owner"), 100),
            Err(Error::Denied)
        ));
        q.revoke("ag_email").unwrap();
        assert!(matches!(
            q.thread(&thread, &member("user_alice"), 100),
            Err(Error::Denied)
        ));
        assert!(!q.dispatchable(&intent, 101).unwrap());
    }
    #[test]
    fn email_lease_reclaim_fences_stale_worker_and_pause_allows_receipt_only() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        q.admit("ag_email", 1, &message("msg_one", "body", &[]), 100)
            .unwrap();
        let old = claim(&mut q, 100, 1);
        assert!(q
            .claim("ag_email", 1, 159, &format!("{:064x}", 2))
            .unwrap()
            .is_none());
        let current = claim(&mut q, 160, 2);
        assert!(!q.dispatchable(&old, 161).unwrap());
        assert!(matches!(q.complete(&old, Mode::Outbox), Err(Error::Lease)));
        assert!(q.dispatchable(&current, 161).unwrap());
        q.store.pause("ag_email").unwrap();
        assert!(!q.dispatchable(&current, 162).unwrap());
        q.complete(&current, Mode::Outbox).unwrap();
        assert!(matches!(
            q.claim("ag_email", 1, 200, &format!("{:064x}", 3)),
            Err(Error::Denied)
        ));
    }
    #[test]
    fn email_retry_backoff_and_attempt_budget_are_bounded() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        q.admit("ag_email", 1, &message("msg_one", "body", &[]), 100)
            .unwrap();
        let first = claim(&mut q, 100, 1);
        q.failed(&first, 100, false).unwrap();
        assert!(q
            .claim("ag_email", 1, 159, &format!("{:064x}", 2))
            .unwrap()
            .is_none());
        for i in 2..=8 {
            let intent = claim(&mut q, i as i64 * 4000, i);
            q.failed(&intent, i as i64 * 4000, false).unwrap();
        }
        assert!(q
            .claim("ag_email", 1, 100000, &format!("{:064x}", 9))
            .unwrap()
            .is_none());
        assert!(matches!(q.failed(&first, -1, false), Err(Error::Invalid)));
        assert!(q
            .claim("ag_email", 1, i64::MAX, &format!("{:064x}", 9))
            .is_err());
    }
    #[test]
    fn email_per_binding_and_global_attempt_limits_fail_closed() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        for i in 0..11 {
            q.admit(
                "ag_email",
                1,
                &message(&format!("msg_{i}"), "body", &[]),
                100,
            )
            .unwrap();
        }
        for n in 0..10 {
            let intent = claim(&mut q, 100, n);
            q.complete(&intent, Mode::Outbox).unwrap();
        }
        assert!(matches!(
            q.claim("ag_email", 1, 100, &format!("{:064x}", 12)),
            Err(Error::Rate)
        ));
        // A separate binding cannot bypass the global attempt window.
        for _ in 0..50 {
            q.store
                .db
                .execute(
                    "INSERT INTO email_attempts(endpoint,at) VALUES('ag_other',100)",
                    [],
                )
                .unwrap();
        }
        assert!(q
            .claim("ag_email", 1, 160, &format!("{:064x}", 12))
            .unwrap()
            .is_some());
        // At 160 the 60-second window expires. Day limit is still enforced.
        for _ in 0..90 {
            q.store
                .db
                .execute(
                    "INSERT INTO email_attempts(endpoint,at) VALUES('ag_email',160)",
                    [],
                )
                .unwrap();
        }
        assert!(matches!(
            q.claim("ag_email", 1, 221, &format!("{:064x}", 13)),
            Err(Error::Rate)
        ));
    }
    #[test]
    fn email_admission_is_atomic_on_queue_failure() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        q.store.db.execute_batch("CREATE TRIGGER reject_intent BEFORE INSERT ON email_intents BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        assert!(q
            .admit("ag_email", 1, &message("msg_one", "body", &[]), 100)
            .is_err());
        for table in ["email_messages", "email_threads", "email_intents"] {
            let n: i64 = q
                .store
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0);
        }
    }
    #[test]
    fn email_outbox_is_private_atomic_idempotent_and_rejects_file_aliases() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        q.admit(
            "ag_email",
            1,
            &message("msg_one", "SYNTHETIC_PRIVATE_BODY", &[]),
            100,
        )
        .unwrap();
        let intent = claim(&mut q, 100, 1);
        write_outbox(&intent, "https://hub.example.test/", &directory).unwrap();
        write_outbox(&intent, "https://hub.example.test/", &directory).unwrap();
        let path = directory.join(format!("{}.json", intent.id));
        let bytes = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("SYNTHETIC_PRIVATE_BODY"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::write(&path, b"changed").unwrap();
        assert!(matches!(
            write_outbox(&intent, "https://hub.example.test/", &directory),
            Err(Error::Conflict)
        ));
        std::fs::write(&path, &bytes).unwrap();
        let alias = directory.join("alias");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(write_outbox(&intent, "https://hub.example.test/", &directory).is_err());
        std::fs::remove_file(&alias).unwrap();
        std::fs::rename(&path, &alias).unwrap();
        std::os::unix::fs::symlink(&alias, &path).unwrap();
        assert!(write_outbox(&intent, "https://hub.example.test/", &directory).is_err());
        assert_eq!(std::fs::read(&alias).unwrap(), bytes);
    }
    #[test]
    fn email_capacity_uses_private_store_limits_and_existing_state_survives_reopen() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let limits = Limits {
            endpoint_count: 1,
            ..Limits::default()
        };
        let thread;
        {
            let mut store = Store::open(&directory, limits).unwrap();
            store.register_paused("ag_email", 1, "ag_internal").unwrap();
            let mut q = Queue::new(&mut store).unwrap();
            q.bind_paused(
                "ag_email",
                1,
                Contact {
                    address: "person@example.test".into(),
                    member_id: "user_alice".into(),
                },
            )
            .unwrap();
            q.store
                .db
                .execute("UPDATE endpoints SET paused=0", [])
                .unwrap();
            thread = q
                .admit("ag_email", 1, &message("msg_one", "private", &[]), 100)
                .unwrap();
            assert!(matches!(
                q.admit("ag_email", 1, &message("msg_two", "another", &[]), 101),
                Err(Error::Full)
            ));
            let _abandoned = claim(&mut q, 100, 1);
        }
        let mut store = Store::open(&directory, limits).unwrap();
        let mut q = Queue::new(&mut store).unwrap();
        assert_eq!(
            q.thread(&thread, &member("user_alice"), 100)
                .unwrap()
                .messages[0]
                .body,
            "private"
        );
        assert!(q
            .claim("ag_email", 1, 159, &format!("{:064x}", 2))
            .unwrap()
            .is_none());
        let retry = claim(&mut q, 160, 2);
        q.complete(&retry, Mode::Outbox).unwrap();
        assert!(q
            .claim("ag_email", 1, 300, &format!("{:064x}", 3))
            .unwrap()
            .is_none());
    }
    #[test]
    fn email_global_caps_apply_even_when_local_counter_is_empty() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        q.admit("ag_email", 1, &message("msg_one", "body", &[]), 100)
            .unwrap();
        for _ in 0..60 {
            q.store
                .db
                .execute(
                    "INSERT INTO email_attempts(endpoint,at) VALUES('ag_other',100)",
                    [],
                )
                .unwrap();
        }
        assert!(matches!(
            q.claim("ag_email", 1, 101, &format!("{:064x}", 1)),
            Err(Error::Rate)
        ));
        q.store
            .db
            .execute("DELETE FROM email_attempts", [])
            .unwrap();
        for _ in 0..1000 {
            q.store
                .db
                .execute(
                    "INSERT INTO email_attempts(endpoint,at) VALUES('ag_other',100)",
                    [],
                )
                .unwrap();
        }
        assert!(matches!(
            q.claim("ag_email", 1, 200, &format!("{:064x}", 1)),
            Err(Error::Rate)
        ));
    }
    #[test]
    fn email_reply_is_member_scoped_fixed_recipient_and_idempotent() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        let thread = q
            .admit("ag_email", 1, &message("msg_one", "body", &[]), 100)
            .unwrap();
        assert!(matches!(
            q.reply(&thread, &member("user_bob"), "key", "reply", 101),
            Err(Error::Denied)
        ));
        let expired = crate::owner_auth::Member::synthetic("user_alice", 101);
        assert!(matches!(
            q.reply(&thread, &expired, "key", "reply", 101),
            Err(Error::Denied)
        ));
        let job = q
            .reply(
                &thread,
                &member("user_alice"),
                "key",
                "reply\n[forged header]\u{202e}\u{2028}\u{2029}",
                101,
            )
            .unwrap();
        assert_eq!(
            job,
            q.reply(
                &thread,
                &member("user_alice"),
                "key",
                "reply\n[forged header]\u{202e}\u{2028}\u{2029}",
                102
            )
            .unwrap()
        );
        assert!(matches!(
            q.reply(&thread, &member("user_alice"), "key", "changed", 102),
            Err(Error::Conflict)
        ));
        let (recipient, body): (String, String) = q
            .store
            .db
            .query_row(
                "SELECT recipient,body FROM inbound WHERE id=?1",
                [job],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(recipient, "ag_internal");
        assert!(body.starts_with("[Workspace member reply; user user_alice; App-asserted]"));
        assert!(body.contains("\n> [forged header]\\u{202e}"));
        assert!(!body.contains('\u{202e}'));
        assert!(!body.contains('\u{2028}'));
        assert!(!body.contains('\u{2029}'));
        assert!(body.contains("\\u{2028}\\u{2029}"));
        let view = q.thread(&thread, &member("user_alice"), 102).unwrap();
        assert_eq!(view.messages.len(), 2);
        assert_eq!(view.messages[1].member_id.as_deref(), Some("user_alice"));
        q.revoke("ag_email").unwrap();
        assert!(q
            .reply(&thread, &member("user_alice"), "other", "reply", 103)
            .is_err());
    }
    #[test]
    fn email_reply_and_queue_commit_together_and_preserve_core_reply_context() {
        let mut store = fixture();
        let mut q = Queue::new(&mut store).unwrap();
        let thread = q
            .admit("ag_email", 1, &message("msg_one", "body", &[]), 100)
            .unwrap();
        q.store.db.execute_batch("CREATE TRIGGER reject_reply BEFORE INSERT ON email_replies BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        assert!(q
            .reply(&thread, &member("user_alice"), "key", "reply", 101)
            .is_err());
        let count: i64 = q
            .store
            .db
            .query_row("SELECT count(*) FROM inbound", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        q.store
            .db
            .execute_batch("DROP TRIGGER reject_reply")
            .unwrap();
        let job = q
            .reply(&thread, &member("user_alice"), "key", "reply", 101)
            .unwrap();
        // Simulate the normal relay ACK storing the actual accepted Core ID.
        q.store
            .db
            .execute(
                "UPDATE inbound SET core_id='msg_human_reply',pending=0 WHERE id=?1",
                [job],
            )
            .unwrap();
        assert_eq!(
            thread,
            q.admit(
                "ag_email",
                1,
                &message("msg_followup", "response", &["msg_human_reply"]),
                102
            )
            .unwrap()
        );
    }
    #[test]
    fn crashed_eighth_send_becomes_terminal_after_lease_expiry() {
        let mut store = Store::memory(crate::store::Limits::default()).unwrap();
        store
            .register_paused("ag_endpoint", 1, "ag_internal")
            .unwrap();
        let mut q = Queue::new(&mut store).unwrap();
        q.bind_paused(
            "ag_endpoint",
            1,
            Contact {
                address: "person@example.test".into(),
                member_id: "human_a".into(),
            },
        )
        .unwrap();
        q.store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        q.admit(
            "ag_endpoint",
            1,
            &Envelope::Outbound {
                delivery_id: "del_one".into(),
                message_id: "msg_one".into(),
                original_sender: crate::protocol::Sender {
                    kind: SenderKind::Agent,
                    id: "ag_internal".into(),
                },
                body: "body".into(),
                context_ids: vec![],
            },
            100,
        )
        .unwrap();
        for i in 0..8 {
            assert!(q
                .claim("ag_endpoint", 1, 100 + i * 1000, &format!("{i:064x}"))
                .unwrap()
                .is_some());
        }
        assert!(q
            .claim("ag_endpoint", 1, 8100, &"f".repeat(64))
            .unwrap()
            .is_none());
        assert_eq!(q.receipt("ag_endpoint", 1, "msg_one").unwrap(), Some(2));
    }
    #[test]
    fn retained_email_reply_does_not_abort_other_channel_terminal_pruning() {
        let mut store = fixture();
        let id = {
            let mut q = Queue::new(&mut store).unwrap();
            let thread = q
                .admit("ag_email", 1, &message("msg_one", "body", &[]), 100)
                .unwrap();
            q.reply(&thread, &member("user_alice"), "reply", "human text", 101)
                .unwrap()
        };
        let lease = "a".repeat(64);
        store.lease("ag_email", 1, 101, &lease).unwrap().unwrap();
        store
            .complete("ag_email", 1, id, &lease, "msg_accepted")
            .unwrap();
        store
            .enqueue(crate::store::Inbound {
                endpoint: "ag_email",
                generation: 1,
                external_id: "other_transport_item",
                recipient: "ag_internal",
                body: "other text",
            })
            .unwrap();
        let other = store.lease("ag_email", 1, 102, &lease).unwrap().unwrap();
        store
            .complete("ag_email", 1, other.job_id, &lease, "msg_other")
            .unwrap();
        store
            .db
            .execute("UPDATE inbound SET terminal_at=100", [])
            .unwrap();
        assert_eq!(
            store
                .prune_completed(crate::transfers::TOMBSTONE_TTL + 100)
                .unwrap(),
            1
        );
        let rows: i64 = store
            .db
            .query_row("SELECT count(*) FROM inbound", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(
            store
                .db
                .query_row("SELECT core_id FROM inbound WHERE id=?1", [id], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "msg_accepted"
        );
    }
}
