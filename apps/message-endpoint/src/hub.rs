//! Private hub receiver. No HTTP/owner authorization or external transport here.
//! Public callers must never be given these provisioning methods directly.
use crate::{
    core::{AgentMetadata, Core, Delivery},
    failures::FailureReason,
    protocol::{Direction, Envelope, SenderKind},
    relay::Mailbox,
    store::{valid_id, Error as StorageError, Store},
    transfers::FragmentReceipt,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const READBACK_TTL: i64 = 30;
const STATUS_INTERVAL: i64 = 60;
const UNKNOWN_SENDER_SLOTS: i64 = 128;

#[derive(Debug, Error)]
pub enum Error {
    #[error("hub_storage_unavailable")]
    Storage(#[from] StorageError),
    #[error("hub_core_unavailable")]
    Core(#[from] crate::core::Error),
    #[error("hub_binding_unavailable")]
    Binding,
    #[error("hub_cli_integrity_failed")]
    Integrity,
    #[error("hub_revision_conflict")]
    Revision,
    #[error("hub_rejection_budget_exhausted")]
    FailureBudget,
    #[error("hub_refetch_rate_limited")]
    RefetchRateLimited,
    #[error("hub_receipt_conflict")]
    ReceiptConflict,
}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.into())
    }
}

pub trait Directory {
    fn integrity_ok(&self) -> bool {
        true
    }
    fn metadata(&self, id: &str) -> Result<AgentMetadata, crate::core::Error>;
}
impl Directory for Core {
    fn integrity_ok(&self) -> bool {
        !self.integrity_failed()
    }
    fn metadata(&self, id: &str) -> Result<AgentMetadata, crate::core::Error> {
        self.agent_metadata(id)
    }
}

/// Owner-provisioned expectations. Paths are drift detection, never authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub endpoint_id: String,
    pub generation: i64,
    pub recipient: String,
    pub expected_path: String,
    pub expected_cwd: String,
    pub expected_server_id: String,
}
/// Internal worker observation: capture requested_at BEFORE querying Core.
/// Never deserialize this object from an owner/browser API request.
pub struct Readback {
    pub observed: Option<AgentMetadata>,
    pub requested_at: i64,
}
#[derive(Serialize)]
pub struct View {
    pub binding: Binding,
    pub revision: i64,
    pub paused: bool,
    pub reason: String,
    pub observed: Option<AgentMetadata>,
    pub checked_at: Option<i64>,
    /// Endpoint-asserted diagnostic, NOT verification of command or Policy.
    pub diagnostic: Option<Status>,
    pub activity: crate::activation_state::Activity,
    pub isolation: Option<crate::recovery::IsolationChoice>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    #[serde(default)]
    forwarded_evictions: u64,
    protocol: String,
    generation: i64,
    release_sha256: String,
    actual_sha256: String,
    pub(crate) version: String,
    pub(crate) state: String,
}
impl Status {
    fn valid(&self, generation: i64) -> bool {
        let hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        self.protocol == "treer.endpoint.status/v1"
            && self.generation == generation
            && hash(&self.release_sha256)
            && hash(&self.actual_sha256)
            && !self.version.is_empty()
            && self.version.len() <= 64
            && !self.version.chars().any(char::is_control)
            && matches!(self.state.as_str(), "running" | "failure_budget_exhausted")
    }
}

pub struct Hub<M> {
    pub(crate) store: Store,
    pub(crate) mailbox: M,
    pub(crate) workspace: String,
    pub(crate) own_id: String,
}
fn stable(id: &str) -> bool {
    valid_id(id) && (id.starts_with("ag_") || id.starts_with("app_"))
}
fn physical_tree_path(path: &str) -> bool {
    path.len() <= 4096
        && path.starts_with('/')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .skip(1)
            .all(|p| !p.is_empty() && p.trim() == p && p != "." && p != "..")
}
impl<M: Mailbox + Directory> Hub<M> {
    /// Pins this hub runtime ID. Restart with a different ID requires human drain,
    /// fresh provisioning and endpoint repinning, never automatic authority transfer.
    pub fn new(mut store: Store, mailbox: M, own_id: &str, workspace: &str) -> Result<Self, Error> {
        if !stable(own_id) || !valid_id(workspace) {
            return Err(Error::Binding);
        }
        let relay: bool = store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='relay_pins')",
            [],
            |r| r.get(0),
        )?;
        if relay {
            return Err(Error::Binding);
        }
        // Initialize/migrate local history before any audited deletion can retire it.
        crate::telegram_reply_store::Mappings::new(&mut store)?;
        crate::telegram_dispositions::initialize(&mut store)?;
        let tx = store.db.transaction()?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS hub_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1), agent TEXT NOT NULL,workspace TEXT NOT NULL,blocked INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS hub_bindings(endpoint TEXT PRIMARY KEY REFERENCES endpoints(id), definition TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,reason TEXT NOT NULL DEFAULT 'owner_paused',observed TEXT,checked_at INTEGER,diagnostic TEXT,diagnostic_at INTEGER);
        CREATE TABLE IF NOT EXISTS hub_core_handoff_health(endpoint TEXT PRIMARY KEY REFERENCES endpoints(id),generation INTEGER NOT NULL,failures INTEGER NOT NULL,last_job INTEGER NOT NULL,last_token TEXT NOT NULL,first_failure_at INTEGER NOT NULL DEFAULT 0,last_failure_at INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS hub_activity(endpoint TEXT PRIMARY KEY REFERENCES endpoints(id),generation INTEGER NOT NULL,activated_at INTEGER NOT NULL,inbound_updates INTEGER NOT NULL DEFAULT 0,outbound_chunks INTEGER NOT NULL DEFAULT 0,stale_updates INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS hub_transport_health(endpoint TEXT PRIMARY KEY REFERENCES endpoints(id),consecutive_parks INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS hub_parked(endpoint TEXT NOT NULL,generation INTEGER NOT NULL,transfer TEXT NOT NULL,reason TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(endpoint,generation,transfer));
        CREATE TABLE IF NOT EXISTS hub_rejections(delivery TEXT PRIMARY KEY,reason TEXT NOT NULL,created_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS hub_unknown_senders(kind TEXT NOT NULL,sender TEXT NOT NULL,attempts INTEGER NOT NULL,last_seen INTEGER NOT NULL,PRIMARY KEY(kind,sender));
        CREATE TABLE IF NOT EXISTS hub_unknown_overflow(singleton INTEGER PRIMARY KEY CHECK(singleton=1),attempts INTEGER NOT NULL,last_seen INTEGER NOT NULL);
        INSERT OR IGNORE INTO hub_unknown_overflow VALUES(1,0,0);")?;
        let health_columns = tx
            .prepare("PRAGMA table_info(hub_core_handoff_health)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !health_columns.iter().any(|name| name == "first_failure_at") {
            tx.execute("ALTER TABLE hub_core_handoff_health ADD COLUMN first_failure_at INTEGER NOT NULL DEFAULT 0",[])?;
        }
        if !health_columns.iter().any(|name| name == "last_failure_at") {
            tx.execute("ALTER TABLE hub_core_handoff_health ADD COLUMN last_failure_at INTEGER NOT NULL DEFAULT 0",[])?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO hub_identity(singleton,agent,workspace) VALUES(1,?1,?2)",
            params![own_id, workspace],
        )?;
        let pins: (String, String) = tx.query_row(
            "SELECT agent,workspace FROM hub_identity WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if pins != (own_id.into(), workspace.into()) {
            return Err(Error::Binding);
        }
        tx.commit()?;
        let hub = Self {
            store,
            mailbox,
            workspace: workspace.into(),
            own_id: own_id.into(),
        };
        #[cfg(feature = "hub")]
        crate::management::schema(&hub.store.db)?;
        hub.notice_schema()?;
        hub.hold_schema()?;
        let telegram: bool = hub.store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='telegram_chunks')",
            [],
            |r| r.get(0),
        )?;
        if telegram {
            hub.telegram_schema()?;
        }
        Ok(hub)
    }
    /// Internal owner-service boundary only. New bindings always start paused.
    #[cfg(test)]
    pub(crate) fn add_binding(&mut self, binding: Binding) -> Result<(), Error> {
        self.add_binding_inner(binding, None, None)
    }
    pub fn add_binding_audited(
        &mut self,
        binding: Binding,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        let context = crate::owner_actions::AuditContext::new(actor, now)?;
        self.recovery_schema()?;
        self.add_binding_inner(binding, Some(&context), None)
    }
    #[cfg(feature = "hub")]
    pub(crate) fn add_binding_idempotent(
        &mut self,
        binding: Binding,
        actor: &str,
        now: i64,
        key: &str,
        fingerprint: &str,
    ) -> Result<(), Error> {
        let context = crate::owner_actions::AuditContext::new(actor, now)?;
        self.recovery_schema()?;
        self.add_binding_inner(binding, Some(&context), Some((actor, key, fingerprint)))
    }
    fn add_binding_inner(
        &mut self,
        binding: Binding,
        audit: Option<&crate::owner_actions::AuditContext<'_>>,
        receipt: Option<(&str, &str, &str)>,
    ) -> Result<(), Error> {
        if !stable(&binding.endpoint_id)
            || !stable(&binding.recipient)
            || binding.generation <= 0
            || !valid_id(&binding.expected_server_id)
            || !physical_tree_path(&binding.expected_path)
            || binding.expected_cwd.is_empty()
            || binding.expected_cwd.len() > 4096
            || binding.expected_cwd.chars().any(char::is_control)
        {
            return Err(Error::Binding);
        }
        let definition = serde_json::to_string(&binding).map_err(|_| Error::Binding)?;
        let tx = self.store.db.transaction()?;
        #[cfg(feature="hub")]
        let count_sql="SELECT COUNT(*) FROM endpoints WHERE NOT EXISTS(SELECT 1 FROM hub_retired_bindings r WHERE r.endpoint=endpoints.id)";
        #[cfg(not(feature = "hub"))]
        let count_sql = "SELECT COUNT(*) FROM endpoints";
        let count: i64 = tx.query_row(count_sql, [], |r| r.get(0))?;
        if count >= 20 {
            return Err(StorageError::Full.into());
        }
        tx.execute(
            "INSERT INTO endpoints(id,generation,recipient,paused) VALUES(?1,?2,?3,1)",
            params![binding.endpoint_id, binding.generation, binding.recipient],
        )?;
        tx.execute(
            "INSERT INTO hub_bindings(endpoint,definition) VALUES(?1,?2)",
            params![binding.endpoint_id, definition],
        )?;
        #[cfg(feature = "hub")]
        if let Some((actor, key, fingerprint)) = receipt {
            tx.execute(
                "INSERT INTO hub_create_requests VALUES(?1,?2,?3,?4)",
                params![actor, key, fingerprint, binding.endpoint_id],
            )?;
        }
        #[cfg(not(feature = "hub"))]
        let _ = receipt;
        crate::owner_actions::record(&tx, audit, "create_binding", &binding.endpoint_id, 0)?;
        tx.commit()?;
        Ok(())
    }
    pub fn view(&self, id: &str) -> Result<View, Error> {
        #[cfg(feature = "hub")]
        if self.store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM hub_retired_bindings WHERE endpoint=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Binding);
        }
        let (definition,revision,paused,reason,observed,checked_at,diagnostic):(String,i64,bool,String,Option<String>,Option<i64>,Option<String>)=self.store.db.query_row(
            "SELECT b.definition,b.revision,e.paused,CASE WHEN h.conflict=1 THEN 'terminal_receipt_conflict' WHEN h.full=1 THEN 'terminal_receipt_full' ELSE b.reason END,b.observed,b.checked_at,b.diagnostic FROM hub_bindings b JOIN endpoints e ON e.id=b.endpoint LEFT JOIN telegram_terminal_holds h ON h.endpoint=b.endpoint WHERE e.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
        Ok(View {
            activity: self.activation_activity(id)?,
            isolation: self.isolation_choice(id)?,
            binding: serde_json::from_str(&definition).map_err(|_| Error::Binding)?,
            revision,
            paused,
            reason,
            observed: observed
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|_| Error::Binding)?,
            checked_at,
            diagnostic: diagnostic
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|_| Error::Binding)?,
        })
    }
    /// Bounded internal worker snapshot; not a workspace directory enumeration.
    pub fn binding_ids(&self) -> Result<Vec<String>, Error> {
        #[cfg(feature = "hub")]
        let sql="SELECT endpoint FROM hub_bindings WHERE NOT EXISTS(SELECT 1 FROM hub_retired_bindings r WHERE r.endpoint=hub_bindings.endpoint) ORDER BY endpoint LIMIT 20";
        #[cfg(not(feature = "hub"))]
        let sql = "SELECT endpoint FROM hub_bindings ORDER BY endpoint LIMIT 20";
        let mut query = self.store.db.prepare(sql)?;
        let rows = query.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
    /// Per-ID lookup only. Failure/drift pauses and is sticky until owner resumes.
    /// Old expected and current observed values remain separately visible.
    #[cfg(test)]
    pub fn refresh(&mut self, id: &str, now: i64) -> Result<(), Error> {
        let revision = self.view(id)?.revision;
        let observed = self.mailbox.metadata(id).ok();
        self.record_readback(
            id,
            revision,
            Readback {
                observed,
                requested_at: now,
            },
            now,
        )
    }
    /// State-only completion of a Core per-ID query. The runtime, never browser
    /// JSON, supplies `observed`; revision fencing rejects an obsolete query.
    pub fn record_readback(
        &mut self,
        id: &str,
        revision: i64,
        readback: Readback,
        now: i64,
    ) -> Result<(), Error> {
        if readback.requested_at < 0
            || now < readback.requested_at
            || now - readback.requested_at > READBACK_TTL
        {
            return Err(Error::Binding);
        }
        let view = self.view(id)?;
        if view.revision != revision {
            return Err(Error::Revision);
        }
        if view.checked_at.is_some_and(|at| at > readback.requested_at) {
            #[cfg(test)]
            eprintln!(
                "test-only readback superseded: requested={} checked={:?}",
                readback.requested_at, view.checked_at
            );
            return Err(Error::Binding);
        }
        let observed = readback.observed;
        let expected = view.binding;
        let reason = match &observed {
            None => "metadata_unavailable",
            Some(o)
                if o.agent_id != id || o.workspace_id != self.workspace || o.kind != "command" =>
            {
                "identity_mismatch"
            }
            Some(o) if o.path.as_deref() != Some(expected.expected_path.as_str()) => "path_changed",
            Some(o)
                if o.cwd != expected.expected_cwd || o.server_id != expected.expected_server_id =>
            {
                "placement_changed"
            }
            Some(_) => "matched",
        };
        let text = observed
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| Error::Binding)?;
        let tx = self.store.db.transaction()?;
        tx.execute(
            "UPDATE hub_bindings SET observed=?2,checked_at=?3 WHERE endpoint=?1",
            params![id, text, readback.requested_at],
        )?;
        if reason != "matched" {
            tx.execute("UPDATE endpoints SET paused=1 WHERE id=?1", [id])?;
            tx.execute("UPDATE hub_bindings SET revision=revision+CASE WHEN reason=?2 THEN 0 ELSE 1 END,reason=?2 WHERE endpoint=?1",params![id,reason])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// HTTP wiring must authenticate owner, CSRF, revision and confirmation first.
    /// This performs a NEW readback but never interprets status as authorization.
    #[cfg(test)]
    pub fn resume(&mut self, id: &str, revision: i64, now: i64) -> Result<(), Error> {
        let observed = self.mailbox.metadata(id).ok();
        self.resume_after_readback(
            id,
            revision,
            Readback {
                observed,
                requested_at: now,
            },
            now,
        )
    }
    /// Owner-service internal API: authenticate owner/CSRF/confirmation first,
    /// fetch Core metadata outside state access, then submit this exact revision.
    #[cfg(test)]
    pub(crate) fn resume_after_readback(
        &mut self,
        id: &str,
        revision: i64,
        readback: Readback,
        now: i64,
    ) -> Result<(), Error> {
        self.resume_after_readback_inner(id, revision, readback, now, None, false)
    }
    #[cfg(test)]
    pub(crate) fn resume_after_readback_audited(
        &mut self,
        id: &str,
        revision: i64,
        readback: Readback,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        let context = crate::owner_actions::AuditContext::new(actor, now)?;
        self.recovery_schema()?;
        self.resume_after_readback_inner(id, revision, readback, now, Some(&context), false)
    }
    #[cfg(feature = "hub")]
    pub(crate) fn resume_with_context(
        &mut self,
        id: &str,
        revision: i64,
        readback: Readback,
        context: crate::owner_actions::AuditContext<'_>,
        now: i64,
        discard_held: bool,
    ) -> Result<(), Error> {
        self.recovery_schema()?;
        self.resume_after_readback_inner(id, revision, readback, now, Some(&context), discard_held)
    }
    #[cfg(any(test, feature = "hub"))]
    fn resume_after_readback_inner(
        &mut self,
        id: &str,
        revision: i64,
        readback: Readback,
        now: i64,
        audit: Option<&crate::owner_actions::AuditContext<'_>>,
        discard_held: bool,
    ) -> Result<(), Error> {
        self.hold_schema()?;
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        self.record_readback(id, revision, readback, now)?;
        let view = self.view(id)?;
        if view.revision != revision {
            return Err(Error::Revision);
        }
        let o = view.observed.ok_or(Error::Binding)?;
        if o.agent_id != id
            || o.workspace_id != self.workspace
            || o.kind != "command"
            || o.path.as_deref() != Some(view.binding.expected_path.as_str())
            || o.cwd != view.binding.expected_cwd
            || o.server_id != view.binding.expected_server_id
        {
            return Err(Error::Binding);
        }
        let tx = self.store.db.transaction()?;
        if !self.mailbox.integrity_ok() {
            return Err(Error::Integrity);
        }
        if let Some(files) = &self.store._private_files {
            crate::private_store::recheck(files)?;
        }
        crate::owner_actions::record(&tx, audit, "resume_binding", id, revision)?;
        if discard_held {
            crate::holds::discard_for_resume(&tx, id, view.binding.generation, now)?;
        }
        tx.execute("INSERT INTO hub_activity(endpoint,generation,activated_at) VALUES(?1,?2,?3) ON CONFLICT(endpoint) DO UPDATE SET generation=excluded.generation,activated_at=excluded.activated_at,inbound_updates=0,outbound_chunks=0,stale_updates=0",params![id,view.binding.generation,now])?;
        tx.execute("UPDATE endpoints SET paused=0 WHERE id=?1", [id])?;
        tx.execute(
            "UPDATE hub_bindings SET revision=revision+1,reason='active' WHERE endpoint=?1",
            [id],
        )?;
        tx.execute(
            "UPDATE hub_transport_health SET consecutive_parks=0 WHERE endpoint=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn pause(&mut self, id: &str, revision: i64) -> Result<(), Error> {
        self.pause_inner(id, revision, None)
    }
    pub fn pause_audited(
        &mut self,
        id: &str,
        revision: i64,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        let context = crate::owner_actions::AuditContext::new(actor, now)?;
        self.recovery_schema()?;
        self.pause_inner(id, revision, Some(&context))
    }
    fn pause_inner(
        &mut self,
        id: &str,
        revision: i64,
        audit: Option<&crate::owner_actions::AuditContext<'_>>,
    ) -> Result<(), Error> {
        if self.view(id)?.revision != revision {
            return Err(Error::Revision);
        }
        let tx = self.store.db.transaction()?;
        crate::owner_actions::record(&tx, audit, "pause_binding", id, revision)?;
        tx.execute("UPDATE endpoints SET paused=1 WHERE id=?1", [id])?;
        tx.execute(
            "UPDATE hub_bindings SET revision=revision+1,reason='owner_paused' WHERE endpoint=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn blocked(&self) -> Result<bool, Error> {
        if !self.mailbox.integrity_ok() {
            return Err(Error::Integrity);
        }
        // Maintenance reports problems, but each admission/handoff also needs
        // its own fail-closed gate before using private persistent state.
        self.store.recheck_private_storage()?;
        Ok(self.store.db.query_row(
            "SELECT blocked FROM hub_identity WHERE singleton=1",
            [],
            |r| r.get(0),
        )?)
    }
    fn reject_unknown(&mut self, delivery: &Delivery, now: i64) -> Result<(), Error> {
        // Unknown principals may never exhaust an exact-delivery ledger and
        // block trusted endpoints. Bounded aggregates count receive attempts,
        // not unique deliveries (an ack retry may count again). No body/digest.
        let kind = match delivery.message.sender.kind {
            SenderKind::Human => "human",
            SenderKind::Agent => "agent",
            SenderKind::Service => "service",
            SenderKind::Machine => "machine",
        };
        let sender = &delivery.message.sender.id;
        let tx = self.store.db.transaction()?;
        let changed=tx.execute("UPDATE hub_unknown_senders SET attempts=CASE WHEN attempts<9223372036854775807 THEN attempts+1 ELSE attempts END,last_seen=?3 WHERE kind=?1 AND sender=?2",params![kind,sender,now])?;
        if changed == 0 {
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM hub_unknown_senders", [], |r| r.get(0))?;
            if count < UNKNOWN_SENDER_SLOTS {
                tx.execute("INSERT INTO hub_unknown_senders(kind,sender,attempts,last_seen) VALUES(?1,?2,1,?3)",params![kind,sender,now])?;
            } else {
                tx.execute("UPDATE hub_unknown_overflow SET attempts=CASE WHEN attempts<9223372036854775807 THEN attempts+1 ELSE attempts END,last_seen=?1 WHERE singleton=1",[now])?;
            }
        }
        // Old scaffold receipts have no sender information; prune them normally.
        // A previously blocked hub is NOT silently unblocked by this migration:
        // that bit might have been set by bound-endpoint poison as well.
        tx.execute(
            "DELETE FROM hub_rejections WHERE created_at<?1",
            [now - crate::transfers::TOMBSTONE_TTL],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn active(&self, id: &str, now: i64) -> Result<View, Error> {
        let view = self.view(id)?;
        if view.paused
            || view
                .checked_at
                .is_none_or(|t| t > now || now - t > READBACK_TTL)
        {
            return Err(Error::Binding);
        }
        Ok(view)
    }
    /// Workers call periodically, independent from channel network requests.
    pub fn maintenance(&mut self, now: i64) -> Result<(), Error> {
        self.store.recheck_private_storage()?;
        self.cleanup_holds(now)?;
        self.store.cleanup_transfers(now)?;
        self.store.prune_completed(now)?;
        self.cleanup_failure_notices(now)?;
        self.cleanup_refetch_rate(now)?;
        let telegram: bool = self.store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='telegram_chunks')",
            [],
            |r| r.get(0),
        )?;
        if telegram {
            self.cleanup_telegram().map_err(|error| match error {
                crate::telegram::Error::Storage(error) => Error::Storage(error),
                _ => Error::Binding,
            })?;
        }
        Ok(())
    }
    /// Backend handle for independent network workers; never await it while
    /// holding the state actor. Core is cloneable and holds no borrowed identity.
    pub fn backend(&self) -> &M {
        &self.mailbox
    }
    #[cfg(test)]
    pub fn receive_once(&mut self, now: i64) -> Result<(), Error> {
        self.maintenance(now)?;
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        let mut failed = None;
        for delivery in self.mailbox.receive(1000)? {
            let result = self
                .admit_core_delivery(&delivery, now)
                .and_then(|()| self.mailbox.ack(&delivery.delivery_id).map_err(Error::from));
            if let Err(error) = result {
                if matches!(error, Error::FailureBudget) {
                    return Err(error);
                }
                if failed.is_none() {
                    failed = Some(error);
                }
            }
        }
        if let Some(error) = failed {
            return Err(error);
        }
        Ok(())
    }
    /// Persist BEFORE the worker acknowledges Core. Success authorizes an ack,
    /// errors leave Core unacked. No receive/send/ack/metadata network calls here.
    pub fn admit_core_delivery(&mut self, delivery: &Delivery, now: i64) -> Result<(), Error> {
        if now < crate::transfers::TOMBSTONE_TTL {
            return Err(Error::Binding);
        }
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        if delivery.recipient.kind != SenderKind::Agent || delivery.recipient.id != self.own_id {
            return Err(Error::Binding);
        }
        let id = &delivery.message.sender.id;
        let bound: bool = self.store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM hub_bindings WHERE endpoint=?1)",
            [id],
            |r| r.get(0),
        )?;
        if delivery.message.sender.kind != SenderKind::Agent || !bound {
            return self.reject_unknown(delivery, now);
        }
        let view = self.view(id)?;
        // Status is display-only even for paused bindings. One stored update per
        // minute per binding prevents repeated untrusted diagnostics growing state.
        if delivery.message.body.len() <= 1024 {
            if let Ok(status) = serde_json::from_str::<Status>(&delivery.message.body) {
                if status.valid(view.binding.generation) {
                    let text = serde_json::to_string(&status).map_err(|_| Error::Binding)?;
                    self.store.db.execute("UPDATE hub_bindings SET diagnostic=?2,diagnostic_at=?3 WHERE endpoint=?1 AND (diagnostic_at IS NULL OR diagnostic_at<=?4)",params![id,text,now,now-STATUS_INTERVAL])?;
                    return Ok(());
                }
            }
        }
        self.hold_schema()?;
        let discarded:Option<String>=self.store.db.query_row("SELECT message FROM hub_capacity_receipts WHERE endpoint=?1 AND generation=?2 AND delivery=?3 AND reason='owner_discarded_hold'",params![id,view.binding.generation,delivery.delivery_id],|r|r.get(0)).optional()?;
        if let Some(message) = discarded {
            return if message == delivery.message.message_id {
                Ok(())
            } else {
                Err(Error::Binding)
            };
        }
        // Only an unavailable/paused/stale binding permits the hold path.
        // Storage failures must propagate: a worker must not acknowledge Core
        // after a failed database read, even if a later hold write could work.
        let inactive = match self.active(id, now) {
            Ok(_) => false,
            Err(Error::Binding) => true,
            Err(error) => return Err(error),
        };
        if inactive {
            return match self.hold_delivery(delivery, now) {
                Err(Error::Binding) => self.record_rejection(
                    id,
                    view.binding.generation,
                    delivery,
                    FailureReason::InvalidEnvelope,
                    now,
                ),
                result => result,
            };
        }
        match self.store.save_fragment(
            id,
            view.binding.generation,
            Direction::Outbound,
            &delivery.message.body,
            now,
        ) {
            Ok(FragmentReceipt::Expired) => self.record_rejection(
                id,
                view.binding.generation,
                delivery,
                FailureReason::TransferExpired,
                now,
            )?,
            Ok(_) => {}
            Err(StorageError::Full) => self.hold_delivery(delivery, now)?,
            Err(StorageError::Invalid | StorageError::Conflict) => self.record_rejection(
                id,
                view.binding.generation,
                delivery,
                FailureReason::InvalidEnvelope,
                now,
            )?,
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
    fn record_rejection(
        &mut self,
        id: &str,
        generation: i64,
        delivery: &Delivery,
        reason: FailureReason,
        now: i64,
    ) -> Result<(), Error> {
        match self.store.record_failure(
            id,
            generation,
            &delivery.delivery_id,
            delivery.message.body.as_bytes(),
            reason,
            now,
        ) {
            Err(StorageError::Full) => {
                self.store
                    .db
                    .execute("UPDATE hub_identity SET blocked=1 WHERE singleton=1", [])?;
                Err(Error::FailureBudget)
            }
            result => Ok(result?),
        }
    }
    /// Durable park keeps original fragments and queue capacity, but later jobs
    /// can run. Owner UI must expose it; only owner may retry/discard explicitly.
    pub(crate) fn park_transport(
        &mut self,
        id: &str,
        generation: i64,
        transfer: &str,
        reason: FailureReason,
        now: i64,
    ) -> Result<(), Error> {
        let code = match reason {
            FailureReason::TransportRejected => "transport_rejected",
            FailureReason::TransportRetriesExhausted => "transport_retries_exhausted",
            _ => return Err(Error::Binding),
        };
        let envelope =
            self.store
                .transfer_payload(id, generation, Direction::Outbound, transfer)?;
        let bytes = serde_json::to_vec(&envelope).map_err(|_| Error::Binding)?;
        let binding = self.view(id)?.binding;
        if binding.generation != generation {
            return Err(Error::Binding);
        }
        // A human retry may later park for a different reason. Preserve the
        // first terminal receipt rather than overwriting it or conflicting with
        // its fixed delivery key; hub_parked carries the current reason.
        let previous: Option<i64> = self.store.db.query_row(
            "SELECT payload_bytes FROM failures WHERE endpoint=?1 AND generation=?2 AND delivery=?3",
            params![id,generation,format!("transport-{transfer}")], |r| r.get(0)
        ).optional()?;
        if previous.is_some_and(|n| n != -1 && n != bytes.len() as i64) {
            return Err(StorageError::Conflict.into());
        }
        let receipt = if previous.is_some() {
            Ok(())
        } else {
            self.store.record_failure(
                id,
                generation,
                &format!("transport-{transfer}"),
                &bytes,
                reason,
                now,
            )
        };
        match receipt {
            Err(StorageError::Full) => {
                self.store
                    .db
                    .execute("UPDATE hub_identity SET blocked=1 WHERE singleton=1", [])?;
                return Err(Error::FailureBudget);
            }
            other => other?,
        }
        let tx = self.store.db.transaction()?;
        let added=tx.execute("INSERT OR IGNORE INTO hub_parked(endpoint,generation,transfer,reason,created_at) VALUES(?1,?2,?3,?4,?5)",params![id,generation,transfer,code,now])?;
        let notice_code = match reason {
            FailureReason::TransportRejected => crate::protocol::NoticeCode::TransportRejected,
            _ => crate::protocol::NoticeCode::TransportRetriesExhausted,
        };
        if added == 1 {
            crate::notices::insert_notice(&tx, &binding, &envelope, transfer, notice_code, now)?;
        }
        if added == 1 {
            tx.execute(
                "INSERT OR IGNORE INTO hub_transport_health(endpoint) VALUES(?1)",
                [id],
            )?;
            tx.execute("UPDATE hub_transport_health SET consecutive_parks=MIN(consecutive_parks+1,3) WHERE endpoint=?1",[id])?;
            let failures: i64 = tx.query_row(
                "SELECT consecutive_parks FROM hub_transport_health WHERE endpoint=?1",
                [id],
                |r| r.get(0),
            )?;
            if failures >= 3 {
                tx.execute("UPDATE endpoints SET paused=1 WHERE id=?1", [id])?;
                tx.execute("UPDATE hub_bindings SET revision=revision+1,reason='transport_failing' WHERE endpoint=?1",[id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    /// Transport worker owns these jobs until downstream acceptance, including
    /// ambiguous sends. It must never mark a job delivered merely after Core ack.
    pub fn ready_outbound(&self, id: &str, now: i64) -> Result<Vec<(String, Envelope)>, Error> {
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        let binding = self.active(id, now)?.binding;
        let mut query=self.store.db.prepare("SELECT t.transfer FROM transfers t WHERE t.endpoint=?1 AND t.generation=?2 AND t.direction=0 AND t.state=1 AND NOT EXISTS(SELECT 1 FROM hub_parked p WHERE p.endpoint=t.endpoint AND p.generation=t.generation AND p.transfer=t.transfer) ORDER BY t.expires,t.rowid LIMIT 100")?;
        let ids = query
            .query_map(params![id, binding.generation], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|transfer| {
                let payload = self.store.transfer_payload(
                    id,
                    binding.generation,
                    Direction::Outbound,
                    &transfer,
                )?;
                Ok((transfer, payload))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Message, Principal};
    use crate::protocol::Sender;
    use crate::store::Limits;
    use std::cell::RefCell;
    use std::rc::Rc;
    const NOW: i64 = 1_800_000_000;
    #[derive(Clone, Default)]
    struct Fake {
        metadata: Rc<RefCell<Option<AgentMetadata>>>,
        calls: Rc<RefCell<Vec<String>>>,
        deliveries: Rc<RefCell<Vec<Delivery>>>,
        acks: Rc<RefCell<Vec<String>>>,
        fail_ack: Rc<RefCell<bool>>,
    }
    impl Directory for Fake {
        fn metadata(&self, id: &str) -> Result<AgentMetadata, crate::core::Error> {
            self.calls.borrow_mut().push(id.into());
            self.metadata
                .borrow()
                .clone()
                .ok_or(crate::core::Error::Unavailable)
        }
    }
    impl Mailbox for Fake {
        fn receive(&self, _: u32) -> Result<Vec<Delivery>, crate::core::Error> {
            Ok(self.deliveries.borrow_mut().drain(..).collect())
        }
        fn ack(&self, id: &str) -> Result<(), crate::core::Error> {
            if *self.fail_ack.borrow() {
                return Err(crate::core::Error::Unavailable);
            }
            self.acks.borrow_mut().push(id.into());
            Ok(())
        }
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<serde_json::Value>,
        ) -> Result<String, crate::core::Error> {
            panic!("hub receiver must not send as another identity")
        }
    }
    fn binding() -> Binding {
        Binding {
            endpoint_id: "ag_endpoint".into(),
            generation: 1,
            recipient: "ag_recipient".into(),
            expected_path: "/team/endpoints/channel".into(),
            expected_cwd: "/workspace/endpoint".into(),
            expected_server_id: "machine_target".into(),
        }
    }
    fn metadata() -> AgentMetadata {
        AgentMetadata {
            agent_id: "ag_endpoint".into(),
            workspace_id: "ws_test".into(),
            server_id: "machine_target".into(),
            kind: "command".into(),
            cwd: "/workspace/endpoint".into(),
            status: "running".into(),
            path: Some("/team/endpoints/channel".into()),
        }
    }
    fn fixture() -> (Hub<Fake>, Fake) {
        let fake = Fake::default();
        *fake.metadata.borrow_mut() = Some(metadata());
        let mut hub = Hub::new(
            Store::memory(Limits::default()).unwrap(),
            fake.clone(),
            "app_hub",
            "ws_test",
        )
        .unwrap();
        hub.add_binding(binding()).unwrap();
        (hub, fake)
    }
    fn activate(hub: &mut Hub<Fake>) {
        hub.resume("ag_endpoint", 1, NOW).unwrap();
    }
    #[test]
    fn unsafe_live_storage_blocks_admission_and_handoffs_without_waiting_for_maintenance() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(tmp.path()).unwrap().join("state");
        let store = Store::open(&path, Limits::default()).unwrap();
        let mut hub = Hub::new(store, Fake::default(), "app_hub", "ws_test").unwrap();
        hub.add_binding(binding()).unwrap();
        assert!(!hub.blocked().unwrap());
        std::fs::set_permissions(
            path.join("hub.sqlite3"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(matches!(
            hub.blocked(),
            Err(Error::Storage(StorageError::PrivateStorage))
        ));
        let delivery = Delivery {
            delivery_id: "delivery_private".into(),
            recipient: crate::core::Principal {
                kind: SenderKind::Agent,
                id: "app_hub".into(),
            },
            message: crate::core::Message {
                message_id: "msg_private".into(),
                sender: crate::core::Principal {
                    kind: SenderKind::Agent,
                    id: "ag_endpoint".into(),
                },
                body: "never admitted".into(),
                context_ids: vec![],
            },
        };
        assert!(matches!(
            hub.admit_core_delivery(&delivery, NOW),
            Err(Error::Storage(StorageError::PrivateStorage))
        ));
        assert!(matches!(
            hub.ready_outbound("ag_endpoint", NOW),
            Err(Error::Storage(StorageError::PrivateStorage))
        ));
    }
    #[test]
    fn owner_actions_commit_revision_and_actor_audit_atomically_without_network() {
        let fake = Fake::default();
        let mut hub = Hub::new(
            Store::memory(Limits::default()).unwrap(),
            fake.clone(),
            "app_hub",
            "ws_test",
        )
        .unwrap();
        hub.add_binding_audited(binding(), "user_owner", NOW)
            .unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        hub.bind_telegram_audited(
            "ag_endpoint",
            1,
            crate::telegram::Target {
                account_id: "bot_fixture".into(),
                chat_id: 7,
                topic_id: None,
                allowed_senders: vec![8],
            },
            "user_owner",
            NOW,
        )
        .unwrap();
        hub.resume_after_readback_audited(
            "ag_endpoint",
            2,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW,
            },
            "user_owner",
            NOW,
        )
        .unwrap();
        assert!(!hub.view("ag_endpoint").unwrap().paused);
        hub.pause_audited("ag_endpoint", 3, "user_owner", NOW)
            .unwrap();
        let view = hub.view("ag_endpoint").unwrap();
        assert!(view.paused);
        assert_eq!(view.revision, 4);
        let audit = hub.owner_audit(None, 100).unwrap();
        assert_eq!(
            audit.iter().map(|a| a.action.as_str()).collect::<Vec<_>>(),
            vec![
                "pause_binding",
                "resume_binding",
                "bind_telegram",
                "create_binding"
            ]
        );
        assert!(audit
            .iter()
            .all(|a| a.actor_id == "user_owner" && a.at == NOW && a.transfer_id.is_empty()));
        assert_eq!(
            audit
                .iter()
                .map(|a| a.previous_revision)
                .collect::<Vec<_>>(),
            vec![3, 2, 1, 0]
        );
        assert!(fake.calls.borrow().is_empty());
        assert!(hub
            .pause_audited("ag_endpoint", 1, "user_owner", NOW)
            .is_err());
        assert_eq!(hub.owner_audit(None, 100).unwrap().len(), 4);
    }
    #[test]
    fn owner_actions_fail_closed_on_invalid_actor_full_audit_and_stale_readback() {
        let (mut hub, _) = fixture();
        assert!(hub.pause_audited("ag_endpoint", 1, "", NOW).is_err());
        assert!(hub
            .pause_audited("ag_endpoint", 1, "user_owner", -1)
            .is_err());
        assert!(hub
            .resume_after_readback_audited(
                "ag_endpoint",
                1,
                Readback {
                    observed: Some(metadata()),
                    requested_at: NOW
                },
                "user_owner",
                NOW + 31
            )
            .is_err());
        hub.recovery_schema().unwrap();
        hub.store.db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) SELECT 'user_owner','fixture','ag_endpoint','',1,1 FROM n;").unwrap();
        assert!(hub
            .resume_after_readback_audited(
                "ag_endpoint",
                1,
                Readback {
                    observed: Some(metadata()),
                    requested_at: NOW
                },
                "user_owner",
                NOW
            )
            .is_err());
        let view = hub.view("ag_endpoint").unwrap();
        assert!(view.paused);
        assert_eq!(view.revision, 1);
        hub.pause_audited("ag_endpoint", 1, "user_owner", NOW)
            .unwrap();
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 2);
        hub.pause_audited("ag_endpoint", 2, "user_owner", NOW + 1)
            .unwrap();
        assert_eq!(hub.owner_audit_capacity().unwrap(), (true, 1));
        let emergency:(String,i64,i64,i64)=hub.store.db.query_row("SELECT actor,revision,at,count FROM hub_emergency_pauses WHERE endpoint='ag_endpoint'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(emergency, ("user_owner".into(), 2, NOW + 1, 2));
        assert_eq!(hub.owner_audit(None, 100).unwrap().len(), 100);
        let mut another = binding();
        another.endpoint_id = "ag_other".into();
        assert!(hub.add_binding_audited(another, "user_owner", NOW).is_err());
        assert_eq!(hub.binding_ids().unwrap(), vec!["ag_endpoint"]);
    }
    #[test]
    fn owner_actions_audit_failure_rolls_back_creation_and_later_sql_failure_rolls_back_audit() {
        let fake = Fake::default();
        let mut hub = Hub::new(
            Store::memory(Limits::default()).unwrap(),
            fake,
            "app_hub",
            "ws_test",
        )
        .unwrap();
        hub.recovery_schema().unwrap();
        hub.store.db.execute_batch("CREATE TRIGGER reject_audit BEFORE INSERT ON hub_owner_audit BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(hub
            .add_binding_audited(binding(), "user_owner", NOW)
            .is_err());
        assert!(hub.binding_ids().unwrap().is_empty());
        hub.store
            .db
            .execute_batch("DROP TRIGGER reject_audit;")
            .unwrap();
        hub.add_binding(binding()).unwrap();
        hub.resume_after_readback(
            "ag_endpoint",
            1,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW,
            },
            NOW,
        )
        .unwrap();
        hub.store.db.execute_batch("CREATE TRIGGER reject_revision BEFORE UPDATE ON hub_bindings BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(hub
            .pause_audited("ag_endpoint", 2, "user_owner", NOW)
            .is_err());
        let view = hub.view("ag_endpoint").unwrap();
        assert!(!view.paused);
        assert_eq!(view.revision, 2);
        assert!(hub.owner_audit(None, 100).unwrap().is_empty());
    }
    #[test]
    fn owner_actions_immutable_target_conflict_rolls_back_new_account_and_audit() {
        let (mut hub, _) = fixture();
        let target = crate::telegram::Target {
            account_id: "bot_fixture".into(),
            chat_id: 7,
            topic_id: None,
            allowed_senders: vec![8],
        };
        hub.bind_telegram_audited("ag_endpoint", 1, target.clone(), "user_owner", NOW)
            .unwrap();
        let mut conflict = target;
        conflict.account_id = "bot_other".into();
        assert!(hub
            .bind_telegram_audited("ag_endpoint", 2, conflict, "user_owner", NOW)
            .is_err());
        assert_eq!(hub.owner_audit(None, 100).unwrap().len(), 1);
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 2);
        let count: i64 = hub
            .store
            .db
            .query_row(
                "SELECT COUNT(*) FROM telegram_accounts WHERE account='bot_other'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
    fn delivery(id: &str, sender: &str, body: &str) -> Delivery {
        Delivery {
            delivery_id: id.into(),
            recipient: Principal {
                kind: SenderKind::Agent,
                id: "app_hub".into(),
            },
            message: Message {
                message_id: format!("msg_{id}"),
                sender: Principal {
                    kind: SenderKind::Agent,
                    id: sender.into(),
                },
                body: body.into(),
                context_ids: vec![],
            },
        }
    }
    fn outbound(body: &str) -> Vec<String> {
        Envelope::Outbound {
            delivery_id: "del_original".into(),
            message_id: "msg_original".into(),
            original_sender: Sender {
                kind: SenderKind::Agent,
                id: "ag_origin".into(),
            },
            body: body.into(),
            context_ids: vec![],
        }
        .fragments("ag_endpoint", 1)
        .unwrap()
    }
    #[test]
    fn owner_activation_is_revision_checked_and_readback_never_auto_resumes() {
        let (mut hub, fake) = fixture();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        hub.refresh("ag_endpoint", NOW).unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        activate(&mut hub);
        assert!(!hub.view("ag_endpoint").unwrap().paused);
        assert!(matches!(hub.pause("ag_endpoint", 1), Err(Error::Revision)));
        hub.pause("ag_endpoint", 2).unwrap();
        hub.refresh("ag_endpoint", NOW + 1).unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        assert!(fake.calls.borrow().iter().all(|id| id == "ag_endpoint"));
    }
    #[test]
    fn drift_displays_old_and_new_path_and_requires_explicit_resume_after_restoration() {
        let (mut hub, fake) = fixture();
        activate(&mut hub);
        fake.metadata.borrow_mut().as_mut().unwrap().path = Some("/other/channel".into());
        hub.refresh("ag_endpoint", NOW + 1).unwrap();
        let view = hub.view("ag_endpoint").unwrap();
        assert!(view.paused);
        assert_eq!(view.reason, "path_changed");
        assert_eq!(view.binding.expected_path, "/team/endpoints/channel");
        assert_eq!(
            view.observed.unwrap().path.as_deref(),
            Some("/other/channel")
        );
        assert!(hub.resume("ag_endpoint", view.revision, NOW + 2).is_err());
        *fake.metadata.borrow_mut() = Some(metadata());
        hub.refresh("ag_endpoint", NOW + 3).unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        hub.resume(
            "ag_endpoint",
            hub.view("ag_endpoint").unwrap().revision,
            NOW + 3,
        )
        .unwrap();
    }
    #[test]
    fn missing_metadata_wrong_workspace_kind_cwd_machine_or_hidden_path_pause() {
        for case in 0..6 {
            let (mut hub, fake) = fixture();
            activate(&mut hub);
            let mut observed = metadata();
            match case {
                0 => {
                    *fake.metadata.borrow_mut() = None;
                }
                1 => observed.workspace_id = "ws_other".into(),
                2 => observed.kind = "app".into(),
                3 => observed.cwd = "/other".into(),
                4 => observed.server_id = "machine_other".into(),
                _ => observed.path = None,
            }
            if case != 0 {
                *fake.metadata.borrow_mut() = Some(observed);
            }
            hub.refresh("ag_endpoint", NOW + 1).unwrap();
            assert!(hub.view("ag_endpoint").unwrap().paused);
            assert!(hub
                .resume(
                    "ag_endpoint",
                    hub.view("ag_endpoint").unwrap().revision,
                    NOW + 2
                )
                .is_err());
        }
    }
    #[test]
    fn bound_fragments_commit_before_ack_and_replay_without_duplicate_jobs() {
        let (mut hub, fake) = fixture();
        activate(&mut hub);
        let wire = outbound(&"body".repeat(8000));
        assert!(wire.len() > 1);
        for (n, body) in wire.iter().enumerate() {
            fake.deliveries
                .borrow_mut()
                .push(delivery(&format!("del_{n}"), "ag_endpoint", body));
        }
        *fake.fail_ack.borrow_mut() = true;
        assert!(hub.receive_once(NOW).is_err());
        assert!(fake.acks.borrow().is_empty());
        assert_eq!(hub.ready_outbound("ag_endpoint", NOW).unwrap().len(), 1);
        *fake.fail_ack.borrow_mut() = false;
        for (n, body) in wire.iter().enumerate() {
            fake.deliveries
                .borrow_mut()
                .push(delivery(&format!("del_{n}"), "ag_endpoint", body));
        }
        hub.receive_once(NOW + 1).unwrap();
        assert_eq!(hub.ready_outbound("ag_endpoint", NOW + 1).unwrap().len(), 1);
        assert_eq!(fake.acks.borrow().len(), wire.len());
    }
    #[test]
    fn storage_read_failure_never_commits_a_hold_or_permits_ack() {
        let (mut hub, fake) = fixture();
        let delivery = delivery("del_storage", "ag_endpoint", &outbound("body")[0]);
        // A damaged binding projection is a storage fault, not a stale or
        // owner-paused binding. Even the paused path must leave Core unacked.
        hub.store
            .db
            .execute("UPDATE hub_bindings SET checked_at='invalid_integer'", [])
            .unwrap();
        assert!(matches!(
            hub.admit_core_delivery(&delivery, NOW),
            Err(Error::Storage(_))
        ));
        assert!(fake.acks.borrow().is_empty());
        let count: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM hub_holds", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
    #[test]
    fn paused_or_stale_binding_holds_before_ack_and_unknown_sender_cannot_route() {
        let (mut hub, fake) = fixture();
        let wire = outbound("body").remove(0);
        fake.deliveries
            .borrow_mut()
            .push(delivery("del_paused", "ag_endpoint", &wire));
        hub.receive_once(NOW).unwrap();
        assert_eq!(fake.acks.borrow().as_slice(), ["del_paused"]);
        assert_eq!(hub.hold_summary("ag_endpoint").unwrap().held_fragments, 1);
        activate(&mut hub);
        fake.deliveries
            .borrow_mut()
            .push(delivery("del_stale", "ag_endpoint", &wire));
        hub.receive_once(NOW + READBACK_TTL + 1).unwrap();
        assert_eq!(fake.acks.borrow().len(), 2);
        assert_eq!(hub.hold_summary("ag_endpoint").unwrap().held_fragments, 2);
        fake.deliveries
            .borrow_mut()
            .push(delivery("del_unknown", "ag_unknown", &wire));
        hub.receive_once(NOW + 1).unwrap();
        assert_eq!(fake.acks.borrow().last().unwrap(), "del_unknown");
        assert!(hub
            .ready_outbound("ag_endpoint", NOW + 1)
            .unwrap()
            .is_empty());
        let count: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM hub_unknown_senders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let mut human = delivery("del_human", "ag_endpoint", &wire);
        human.message.sender.kind = SenderKind::Human;
        fake.deliveries.borrow_mut().push(human);
        hub.receive_once(NOW + 1).unwrap();
        assert!(hub
            .ready_outbound("ag_endpoint", NOW + 1)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn status_is_rate_limited_diagnostic_and_cannot_activate_or_repin() {
        let (mut hub, fake) = fixture();
        let status = |hash: &str| {
            serde_json::json!({"protocol":"treer.endpoint.status/v1","generation":1,"release_sha256":"a".repeat(64),"actual_sha256":hash,"version":"0.1.0","state":"running"}).to_string()
        };
        fake.deliveries.borrow_mut().push(delivery(
            "del_status",
            "ag_endpoint",
            &status(&"a".repeat(64)),
        ));
        hub.receive_once(NOW).unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        fake.deliveries.borrow_mut().push(delivery(
            "del_status2",
            "ag_endpoint",
            &status(&"b".repeat(64)),
        ));
        hub.receive_once(NOW + 1).unwrap();
        assert_eq!(
            hub.view("ag_endpoint")
                .unwrap()
                .diagnostic
                .unwrap()
                .actual_sha256,
            "a".repeat(64)
        );
        fake.deliveries.borrow_mut().push(delivery(
            "del_status3",
            "ag_endpoint",
            &status(&"b".repeat(64)),
        ));
        hub.receive_once(NOW + 61).unwrap();
        assert_eq!(
            hub.view("ag_endpoint")
                .unwrap()
                .diagnostic
                .unwrap()
                .actual_sha256,
            "b".repeat(64)
        );
        assert!(hub.view("ag_endpoint").unwrap().paused);
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 1);
    }
    #[test]
    fn poison_is_committed_without_body_and_full_budget_blocks_without_ack() {
        let (mut hub, fake) = fixture();
        activate(&mut hub);
        fake.deliveries.borrow_mut().push(delivery(
            "del_poison",
            "ag_endpoint",
            "not a fragment secret",
        ));
        hub.receive_once(NOW).unwrap();
        assert_eq!(
            hub.store
                .recent_failures("ag_endpoint", 1, 100)
                .unwrap()
                .len(),
            1
        );
        for i in 1..1000 {
            hub.store
                .record_failure(
                    "ag_endpoint",
                    1,
                    &format!("del_poison_{i}"),
                    b"bad",
                    FailureReason::InvalidEnvelope,
                    NOW,
                )
                .unwrap();
        }
        fake.deliveries
            .borrow_mut()
            .push(delivery("del_full", "ag_endpoint", "bad"));
        assert!(matches!(
            hub.receive_once(NOW + 1),
            Err(Error::FailureBudget)
        ));
        assert!(!fake.acks.borrow().contains(&"del_full".into()));
        assert!(hub.blocked().unwrap());
    }
    #[test]
    fn unknown_flood_uses_bounded_sender_aggregates_without_blocking_bound_work() {
        let (mut hub, fake) = fixture();
        activate(&mut hub);
        for i in 0..2000 {
            fake.deliveries.borrow_mut().push(delivery(
                &format!("del_unknown_{i}"),
                &format!("ag_unknown_{i}"),
                "untrusted",
            ));
        }
        hub.receive_once(NOW).unwrap();
        assert!(!hub.blocked().unwrap());
        assert_eq!(fake.acks.borrow().len(), 2000);
        let rows: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM hub_unknown_senders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, UNKNOWN_SENDER_SLOTS);
        let overflow: i64 = hub
            .store
            .db
            .query_row("SELECT attempts FROM hub_unknown_overflow", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(overflow, 2000 - UNKNOWN_SENDER_SLOTS);
        let wire = outbound("bound body").remove(0);
        fake.deliveries
            .borrow_mut()
            .push(delivery("del_bound", "ag_endpoint", &wire));
        hub.receive_once(NOW + 1).unwrap();
        assert_eq!(hub.ready_outbound("ag_endpoint", NOW + 1).unwrap().len(), 1);
        assert!(fake.acks.borrow().contains(&"del_bound".into()));
    }
    #[test]
    fn wrong_recipient_never_routes_or_acks_even_under_faulty_mailbox_adapter() {
        let (mut hub, fake) = fixture();
        activate(&mut hub);
        let wire = outbound("body").remove(0);
        let mut wrong = delivery("del_wrong", "ag_endpoint", &wire);
        wrong.recipient.id = "app_other".into();
        fake.deliveries.borrow_mut().push(wrong);
        assert!(hub.receive_once(NOW).is_err());
        assert!(fake.acks.borrow().is_empty());
        assert!(hub.ready_outbound("ag_endpoint", NOW).unwrap().is_empty());
    }
    #[test]
    fn state_admission_readback_and_maintenance_never_call_network_backend() {
        struct NoIo;
        impl Directory for NoIo {
            fn metadata(&self, _: &str) -> Result<AgentMetadata, crate::core::Error> {
                panic!("metadata in state actor")
            }
        }
        impl Mailbox for NoIo {
            fn receive(&self, _: u32) -> Result<Vec<Delivery>, crate::core::Error> {
                panic!("receive in state actor")
            }
            fn ack(&self, _: &str) -> Result<(), crate::core::Error> {
                panic!("ack in state actor")
            }
            fn send(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: &[String],
                _: Option<serde_json::Value>,
            ) -> Result<String, crate::core::Error> {
                panic!("send in state actor")
            }
        }
        let mut hub = Hub::new(
            Store::memory(Limits::default()).unwrap(),
            NoIo,
            "app_hub",
            "ws_test",
        )
        .unwrap();
        hub.add_binding(binding()).unwrap();
        hub.resume_after_readback(
            "ag_endpoint",
            1,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW,
            },
            NOW,
        )
        .unwrap();
        hub.maintenance(NOW).unwrap();
        let wire = outbound("body").remove(0);
        hub.admit_core_delivery(&delivery("del_state", "ag_endpoint", &wire), NOW)
            .unwrap();
        assert_eq!(hub.ready_outbound("ag_endpoint", NOW).unwrap().len(), 1);
        hub.admit_core_delivery(&delivery("del_unknown", "ag_unknown", "untrusted"), NOW)
            .unwrap();
        // Ack intentionally stays with the worker AFTER these commits.
    }
    #[test]
    fn delayed_readback_does_not_extend_freshness_or_resume_and_cannot_replace_newer_query() {
        let (mut hub, _) = fixture();
        assert!(hub
            .resume_after_readback(
                "ag_endpoint",
                1,
                Readback {
                    observed: Some(metadata()),
                    requested_at: NOW
                },
                NOW + 31
            )
            .is_err());
        assert!(hub.view("ag_endpoint").unwrap().paused);
        hub.resume_after_readback(
            "ag_endpoint",
            1,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW,
            },
            NOW + 10,
        )
        .unwrap();
        assert_eq!(hub.view("ag_endpoint").unwrap().checked_at, Some(NOW));
        assert!(hub.active("ag_endpoint", NOW + 31).is_err());
        hub.record_readback(
            "ag_endpoint",
            2,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW + 20,
            },
            NOW + 21,
        )
        .unwrap();
        let mut older = metadata();
        older.path = Some("/stale/path".into());
        assert!(hub
            .record_readback(
                "ag_endpoint",
                2,
                Readback {
                    observed: Some(older),
                    requested_at: NOW + 19
                },
                NOW + 22
            )
            .is_err());
        assert!(!hub.view("ag_endpoint").unwrap().paused);
        assert_eq!(hub.view("ag_endpoint").unwrap().checked_at, Some(NOW + 20));
        assert!(hub
            .record_readback(
                "ag_endpoint",
                2,
                Readback {
                    observed: Some(metadata()),
                    requested_at: NOW + 23
                },
                NOW + 22
            )
            .is_err());
    }
    #[test]
    fn stale_metadata_completion_cannot_override_newer_owner_pause() {
        let (mut hub, _) = fixture();
        activate(&mut hub);
        let revision = hub.view("ag_endpoint").unwrap().revision;
        hub.pause("ag_endpoint", revision).unwrap();
        assert!(matches!(
            hub.record_readback(
                "ag_endpoint",
                revision,
                Readback {
                    observed: Some(metadata()),
                    requested_at: NOW + 1
                },
                NOW + 1
            ),
            Err(Error::Revision)
        ));
        assert!(matches!(
            hub.resume_after_readback(
                "ag_endpoint",
                revision,
                Readback {
                    observed: Some(metadata()),
                    requested_at: NOW + 1
                },
                NOW + 1
            ),
            Err(Error::Revision)
        ));
        let view = hub.view("ag_endpoint").unwrap();
        assert!(view.paused);
        assert_eq!(view.reason, "owner_paused");
    }
    #[test]
    fn restart_keeps_partial_fragments_pins_and_pause_state() {
        let temporary = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(temporary.path()).unwrap().join("hub");
        let fake = Fake::default();
        *fake.metadata.borrow_mut() = Some(metadata());
        let mut hub = Hub::new(
            Store::open(&dir, Limits::default()).unwrap(),
            fake.clone(),
            "app_hub",
            "ws_test",
        )
        .unwrap();
        hub.add_binding(binding()).unwrap();
        activate(&mut hub);
        let wire = outbound(&"body".repeat(8000));
        fake.deliveries
            .borrow_mut()
            .push(delivery("del_first", "ag_endpoint", &wire[0]));
        hub.receive_once(NOW).unwrap();
        drop(hub);
        let mut hub = Hub::new(
            Store::open(&dir, Limits::default()).unwrap(),
            fake.clone(),
            "app_hub",
            "ws_test",
        )
        .unwrap();
        for (i, body) in wire.iter().enumerate().skip(1) {
            fake.deliveries.borrow_mut().push(delivery(
                &format!("del_next_{i}"),
                "ag_endpoint",
                body,
            ));
        }
        hub.receive_once(NOW + 1).unwrap();
        assert_eq!(hub.ready_outbound("ag_endpoint", NOW + 1).unwrap().len(), 1);
        hub.pause("ag_endpoint", 2).unwrap();
        drop(hub);
        let store = Store::open(&dir, Limits::default()).unwrap();
        assert!(Hub::new(store, fake.clone(), "app_other", "ws_test").is_err());
        let hub = Hub::new(
            Store::open(&dir, Limits::default()).unwrap(),
            fake,
            "app_hub",
            "ws_test",
        )
        .unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
    }
}
