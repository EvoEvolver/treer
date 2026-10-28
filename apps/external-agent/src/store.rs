use crate::{private_store, Error};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{path::Path, time::SystemTime};
use subtle::ConstantTimeEq;

const APP_ID: i64 = 0x54454131;
const DAY: i64 = 86_400;

/// The App identity a store belongs to. Token verifiers are keyed by a random
/// per-store secret kept in `settings.binding`, so configuration edits and
/// Treer updates never invalidate issued tokens.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Binding {
    pub workspace: String,
    pub app: String,
    pub agent: String,
}
impl Binding {
    fn validate(&self) -> Result<(), Error> {
        if [&self.workspace, &self.app, &self.agent]
            .iter()
            .all(|v| identifier(v))
        {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
}

/// Verified per-request owner proof. No public constructor or deserialization.
/// The configured Proxy verifier produces this outside every Store lock.
pub struct Owner<'a> {
    pub(crate) binding: &'a Binding,
    pub(crate) subject: &'a str,
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub live_tokens: i64,
    pub token_records: i64,
    pub audit_rows: i64,
    pub audit_per_token: i64,
    pub concurrent: i64,
    pub concurrent_per_token: i64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            live_tokens: 100,
            token_records: 1000,
            audit_rows: 100_000,
            audit_per_token: 1000,
            concurrent: 8,
            concurrent_per_token: 2,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<(), Error> {
        let max = Self::default();
        if [
            (self.live_tokens, max.live_tokens),
            (self.token_records, max.token_records),
            (self.audit_rows, max.audit_rows),
            (self.audit_per_token, max.audit_per_token),
            (self.concurrent, max.concurrent),
            (self.concurrent_per_token, max.concurrent_per_token),
        ]
        .iter()
        .any(|(v, m)| *v <= 0 || v > m)
        {
            Err(Error::Invalid)
        } else {
            Ok(())
        }
    }
}

/// No Clone/Debug/serde: plaintext can be consumed only for the one create response.
pub struct Created {
    pub record: TokenRecord,
    plaintext: String,
}
impl Created {
    pub fn into_plaintext(self) -> String {
        self.plaintext
    }
}
#[derive(Debug, serde::Serialize)]
pub struct TokenRecord {
    pub id: String,
    pub name: String,
    #[serde(serialize_with = "timestamp")]
    pub created: i64,
    #[serde(serialize_with = "timestamp")]
    pub expires: i64,
    pub revoked: bool,
    #[serde(serialize_with = "optional_timestamp")]
    pub last_used: Option<i64>,
    /// Transport ingress peer only; arbitrary forwarded-client headers are ignored.
    pub ingress_peer: Option<String>,
}

fn timestamp<S: serde::Serializer>(v: &i64, s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::Error;
    let v = chrono::DateTime::<chrono::Utc>::from_timestamp(*v, 0)
        .ok_or_else(|| S::Error::custom("invalid timestamp"))?;
    s.serialize_str(&v.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}
fn optional_timestamp<S: serde::Serializer>(v: &Option<i64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => timestamp(v, s),
        None => s.serialize_none(),
    }
}

/// Audited external operation. Historical rows may also carry the retired typed
/// agent-operation codes (list/status/output/...), which the schema still accepts.
#[derive(Clone, Copy)]
pub enum Operation {
    Registry,
    Inbox,
    Ack,
    Cli,
}
impl Operation {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Registry => "registry",
            Self::Inbox => "inbox",
            Self::Ack => "ack",
        }
    }
}

/// Admission is not authorization by Policy, nor permission to dispatch later.
/// Owns no Store or transaction, retains no credential, cannot be cloned/decoded.
pub struct Reserved {
    request: String,
    token: String,
    generation: String,
    binding: Vec<u8>,
    operation: Operation,
    target: Option<String>,
}
pub struct Dispatched(Reserved);
impl Dispatched {
    pub(crate) fn operation(&self) -> Operation {
        self.0.operation
    }
    pub(crate) fn target(&self) -> Option<&str> {
        self.0.target.as_deref()
    }
    pub fn request_id(&self) -> &str {
        &self.0.request
    }
    pub fn token_id(&self) -> &str {
        &self.0.token
    }
}
#[derive(Clone, Copy)]
pub enum Outcome {
    Completed,
    PolicyDenied,
    Unavailable,
    Unknown,
}
impl Outcome {
    fn code(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::PolicyDenied => "policy_denied",
            Self::Unavailable => "unavailable",
            Self::Unknown => "outcome_unknown",
        }
    }
}

pub struct Store {
    // Connection MUST close before PrivateFiles closes its pinned DB descriptor.
    db: Connection,
    files: private_store::PrivateFiles,
    binding: Binding,
    digest: Vec<u8>,
    limits: Limits,
}

impl Store {
    pub fn open(path: &Path, binding: Binding, limits: Limits) -> Result<Self, Error> {
        limits.validate()?;
        binding.validate()?;
        let (db, files) = private_store::open(path)?;
        // Establish field drop order (Connection before PrivateFiles) before any fallible step.
        let mut store = Self {
            db,
            files,
            binding,
            digest: Vec::new(),
            limits,
        };
        let binding = store.binding.clone();
        let db = &store.db;
        db.busy_timeout(std::time::Duration::from_millis(250))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let app: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let count: i64 = db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )?;
        if !((version == 0 && app == 0 && count == 0)
            || (matches!(version, 1..=6) && app == APP_ID))
        {
            return Err(Error::PrivateStorage);
        }
        // A v6 store records its identity: refuse a different App before touching anything.
        if version == 6 && !same_identity(db, &binding)? {
            return Err(Error::PrivateStorage);
        }
        if matches!(version, 1..=3) {
            db.execute_batch(include_str!("migration-inbox.sql"))?;
        }
        db.execute_batch(include_str!("schema.sql"))?;
        private_store::enable_wal(db, path)?;
        let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
        let existing: Option<Vec<u8>> = tx
            .query_row("SELECT binding FROM settings WHERE id=1", [], |r| r.get(0))
            .optional()?;
        let digest = match existing {
            Some(digest) => digest,
            None => {
                let mut digest = vec![0; 32];
                getrandom::fill(&mut digest).map_err(|_| Error::PrivateStorage)?;
                tx.execute("INSERT INTO settings(id,binding,generation,reviewed,high_water) VALUES(1,?1,?2,0,0)", params![digest, random_hex(32)?])?;
                digest
            }
        };
        // Stores from before schema v6 have no recorded identity: adopt the configured one.
        tx.execute(
            "INSERT OR IGNORE INTO identity(id,workspace,app,agent) VALUES(1,?1,?2,?3)",
            params![binding.workspace, binding.app, binding.agent],
        )?;
        if !same_identity(&tx, &binding)? {
            return Err(Error::PrivateStorage);
        }
        // A previous process may have dispatched a mutation and lost the result.
        // Do not retry, infer failure, or remove incomplete history on restart.
        tx.execute("UPDATE execution_audit SET outcome='outcome_unknown',ended=NULL WHERE outcome IN ('reserved','dispatched')", [])?;
        tx.commit()?;
        store.digest = digest;
        store.recheck()?;
        Ok(store)
    }
    #[cfg(test)]
    pub(crate) fn test_db(&self) -> &Connection {
        &self.db
    }
    pub fn recheck(&self) -> Result<(), Error> {
        private_store::recheck(&self.files)
    }
    fn owner(&self, owner: &Owner<'_>) -> Result<(), Error> {
        self.recheck()?;
        if owner.binding != &self.binding || !identifier(owner.subject) {
            return Err(Error::Denied);
        }
        Ok(())
    }

    pub fn create(&mut self, owner: &Owner<'_>, name: &str, days: u16) -> Result<Created, Error> {
        self.owner(owner)?;
        if !valid_name(name) || !matches!(days, 30 | 90) {
            return Err(Error::Invalid);
        }
        self.create_with_ttl(owner, name, i64::from(days) * DAY)
    }
    pub(crate) fn create_with_ttl(
        &mut self,
        owner: &Owner<'_>,
        name: &str,
        ttl: i64,
    ) -> Result<Created, Error> {
        self.create_scoped(owner, name, ttl, None)
    }
    fn create_scoped(
        &mut self,
        owner: &Owner<'_>,
        name: &str,
        ttl: i64,
        endpoint: Option<(&str, &str, bool)>,
    ) -> Result<Created, Error> {
        self.create_scoped_claim(owner, name, ttl, endpoint, None)
    }
    fn create_scoped_claim(
        &mut self,
        owner: &Owner<'_>,
        name: &str,
        ttl: i64,
        endpoint: Option<(&str, &str, bool)>,
        device: Option<&str>,
    ) -> Result<Created, Error> {
        self.owner(owner)?;
        if !valid_name(name) || !(1..=90 * DAY).contains(&ttl) {
            return Err(Error::Invalid);
        }
        let id = random_hex(16)?;
        let secret = random_hex(32)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        let (live,total):(i64,i64) = tx.query_row("SELECT coalesce(sum(revoked_at IS NULL AND expires>?1 AND generation=(SELECT generation FROM settings WHERE id=1)),0),count(*) FROM tokens",[now],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if live >= self.limits.live_tokens || total >= self.limits.token_records {
            return Err(Error::Capacity);
        }
        let generation = generation(&tx)?;
        if let Some(device) = device {
            let changed=tx.execute("UPDATE device_requests SET status='claimed',token_id=?4 WHERE device_hash=?1 AND status='approved' AND expires>?2 AND generation=?3",params![device,now,generation,id])?;
            if changed != 1 {
                return Err(Error::Denied);
            }
        }
        let verifier = verifier(&self.digest, &generation, &id, &secret);
        let expires = now.checked_add(ttl).ok_or(Error::Invalid)?;
        tx.execute("INSERT INTO tokens(id,name,verifier,generation,created,expires,creator) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,name,verifier,generation,now,expires,owner.subject])?;
        if let Some((key, display, register)) = endpoint {
            if !external_key(key) || !valid_name(display) {
                return Err(Error::Invalid);
            }
            if register {
                let count: i64 =
                    tx.query_row("SELECT count(*) FROM external_agents", [], |r| r.get(0))?;
                if count >= 100 {
                    return Err(Error::Capacity);
                }
                // Keys are immutable and never reused, including disabled registrations.
                tx.execute(
                    "INSERT INTO external_agents(key,name,enabled,created) VALUES(?1,?2,1,?3)",
                    params![key, display, now],
                )
                .map_err(|_| Error::Invalid)?;
            } else if !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM external_agents WHERE key=?1 AND enabled=1)",
                [key],
                |r| r.get::<_, bool>(0),
            )? {
                return Err(Error::Invalid);
            }
            tx.execute(
                "INSERT INTO external_token_bindings(token,endpoint) VALUES(?1,?2)",
                params![id, key],
            )?;
        }
        management(&tx, owner.subject, "created", &id, now)?;
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(Created {
            record: TokenRecord {
                id: id.clone(),
                name: name.into(),
                created: now,
                expires,
                revoked: false,
                last_used: None,
                ingress_peer: None,
            },
            plaintext: format!("ext1_{id}.{secret}"),
        })
    }
    #[cfg(test)]
    pub(crate) fn db_creator(&self, id: &str) -> Result<String, Error> {
        Ok(self
            .db
            .query_row("SELECT creator FROM tokens WHERE id=?1", [id], |r| r.get(0))?)
    }
    pub fn list(&self, owner: &Owner<'_>) -> Result<Vec<TokenRecord>, Error> {
        self.owner(owner)?;
        let mut stmt = self.db.prepare("SELECT id,name,created,expires,revoked_at IS NOT NULL,last_used,(SELECT peer FROM token_sources WHERE token=tokens.id) FROM tokens ORDER BY created,id LIMIT 1000")?;
        let rows = stmt.query_map([], |r| {
            Ok(TokenRecord {
                id: r.get(0)?,
                name: r.get(1)?,
                created: r.get(2)?,
                expires: r.get(3)?,
                revoked: r.get(4)?,
                last_used: r.get(5)?,
                ingress_peer: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
    pub fn revoke(&mut self, owner: &Owner<'_>, id: &str) -> Result<(), Error> {
        self.owner(owner)?;
        if !hex(id, 32) {
            return Err(Error::Invalid);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        let changed = tx.execute(
            "UPDATE tokens SET revoked_at=?1,revoker=?2 WHERE id=?3 AND revoked_at IS NULL",
            params![now, owner.subject, id],
        )?;
        if changed == 1 {
            management(&tx, owner.subject, "revoked", id, now)?;
        }
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(())
    }
    /// The owner HTTP boundary requires a configured acceptance pin and fresh owner acknowledgement.
    /// The retained acceptance evidence must prove real enforcing Policy negatives before deployment.
    pub fn policy_reviewed(&mut self, owner: &Owner<'_>, enabled: bool) -> Result<(), Error> {
        self.owner(owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        tx.execute("UPDATE settings SET reviewed=?1 WHERE id=1", [enabled])?;
        management(
            &tx,
            owner.subject,
            if enabled {
                "policy_reviewed"
            } else {
                "disabled"
            },
            "",
            now,
        )?;
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(())
    }
    /// Offline restore requires calling this before any admission; never reuse
    /// restored credentials. It does not make full same-UID rollback detectable.
    pub fn rotate_after_restore(&mut self, owner: &Owner<'_>) -> Result<(), Error> {
        self.owner(owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        tx.execute(
            "UPDATE settings SET generation=?1,reviewed=0 WHERE id=1",
            [random_hex(32)?],
        )?;
        tx.execute(
            "UPDATE tokens SET revoked_at=coalesce(revoked_at,?1),revoker=coalesce(revoker,?2)",
            params![now, owner.subject],
        )?;
        management(&tx, owner.subject, "rotated", "", now)?;
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(())
    }

    pub fn admit(&mut self, credential: &str, operation: Operation) -> Result<Reserved, Error> {
        self.admit_from(credential, operation, None)
    }
    pub(crate) fn admit_from(
        &mut self,
        credential: &str,
        operation: Operation,
        peer: Option<std::net::IpAddr>,
    ) -> Result<Reserved, Error> {
        self.recheck()?;
        if !self
            .db
            .query_row("SELECT reviewed FROM settings WHERE id=1", [], |r| {
                r.get::<_, bool>(0)
            })?
        {
            return Err(Error::Disabled);
        }
        let (id, secret) = parse(credential)?;
        // One read-only snapshot for invalid credentials: no IMMEDIATE writer lock.
        // The identical credential checks below run again after acquiring that lock.
        type CredentialSnapshot = (
            String,
            i64,
            Option<Vec<u8>>,
            Option<String>,
            Option<i64>,
            Option<bool>,
        );
        let (gen,high,expected,token_gen,expires,revoked):CredentialSnapshot = self.db.query_row(
            "SELECT s.generation,s.high_water,t.verifier,t.generation,t.expires,t.revoked_at IS NOT NULL FROM settings s LEFT JOIN tokens t ON t.id=?1 WHERE s.id=1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        let wall = i64::try_from(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| Error::Invalid)?
                .as_secs(),
        )
        .map_err(|_| Error::Invalid)?;
        let matches = verifier(&self.digest, &gen, id, secret)
            .ct_eq(expected.as_deref().unwrap_or(&[0; 32]))
            .unwrap_u8()
            == 1;
        if !matches
            || token_gen.as_deref() != Some(gen.as_str())
            || expires.is_none_or(|t| t <= wall.max(high))
            || revoked != Some(false)
        {
            return Err(Error::Denied);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Scope is durable token metadata, never an external header or selected target.
        let endpoint: Option<String> = tx
            .query_row(
                "SELECT endpoint FROM external_token_bindings WHERE token=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let scoped_operation = matches!(operation, Operation::Inbox | Operation::Ack);
        if let Some(key) = &endpoint {
            let enabled: bool = tx.query_row(
                "SELECT enabled FROM external_agents WHERE key=?1",
                [key],
                |r| r.get(0),
            )?;
            if !enabled || matches!(operation, Operation::Registry) {
                return Err(Error::Denied);
            }
        } else if scoped_operation {
            return Err(Error::Invalid);
        }
        let target = if scoped_operation {
            endpoint.as_deref()
        } else {
            None
        };
        // Clock sampled after acquiring the write lock. Failed admissions roll
        // back without changing high_water, last_used, audit or any other row.
        let now = sample_time(&tx)?;
        let generation = generation(&tx)?;
        let row = tx
            .query_row(
                "SELECT verifier,generation,expires,revoked_at IS NOT NULL FROM tokens WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, bool>(3)?,
                    ))
                },
            )
            .optional()?;
        let expected = row.as_ref().map(|v| v.0.as_slice()).unwrap_or(&[0; 32]);
        let actual = verifier(&self.digest, &generation, id, secret);
        let valid = actual.ct_eq(expected).unwrap_u8() == 1;
        if !valid || !row.is_some_and(|v| v.1 == generation && v.2 > now && !v.3) {
            return Err(Error::Denied);
        }
        if !reviewed(&tx)? {
            return Err(Error::Disabled);
        }
        // Only stale, NEVER dispatched reservations can be released automatically.
        tx.execute("UPDATE execution_audit SET outcome='unavailable',ended=?1 WHERE outcome='reserved' AND started<=?2",params![now,now.saturating_sub(30)])?;
        // Roll completed rows only. Unknown and all in-flight evidence survives capacity pressure.
        tx.execute("DELETE FROM execution_audit WHERE request IN (SELECT request FROM execution_audit WHERE token=?1 AND outcome IN ('completed','policy_denied','unavailable') AND ended IS NOT NULL AND started<=?2 ORDER BY started,request LIMIT max(0,(SELECT count(*) FROM execution_audit WHERE token=?1)-?3+1))",params![id,now.saturating_sub(60),self.limits.audit_per_token])?;
        tx.execute("DELETE FROM execution_audit WHERE request IN (SELECT request FROM execution_audit WHERE outcome IN ('completed','policy_denied','unavailable') AND ended IS NOT NULL AND started<=?1 ORDER BY started,request LIMIT max(0,(SELECT count(*) FROM execution_audit)-?2+1))",params![now.saturating_sub(60),self.limits.audit_rows])?;
        let (global,local,active,local_active):(i64,i64,i64,i64)=tx.query_row("SELECT count(*),coalesce(sum(token=?1),0),coalesce(sum(outcome IN ('reserved','dispatched')),0),coalesce(sum(token=?1 AND outcome IN ('reserved','dispatched')),0) FROM execution_audit",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if global >= self.limits.audit_rows
            || local >= self.limits.audit_per_token
            || active >= self.limits.concurrent
            || local_active >= self.limits.concurrent_per_token
        {
            return Err(Error::Capacity);
        }
        let recent: i64 = tx.query_row(
            "SELECT count(*) FROM execution_audit WHERE token=?1 AND started>?2",
            params![id, now.saturating_sub(60)],
            |r| r.get(0),
        )?;
        if recent >= 60 {
            return Err(Error::Capacity);
        }
        let request = random_hex(16)?;
        tx.execute("INSERT INTO execution_audit(request,token,generation,operation,target,started,outcome) VALUES(?1,?2,?3,?4,?5,?6,'reserved')",params![request,id,generation,operation.code(),target,now])?;
        tx.execute(
            "UPDATE tokens SET last_used=?1 WHERE id=?2",
            params![now, id],
        )?;
        if let Some(peer) = peer {
            tx.execute("INSERT INTO token_sources(token,peer) VALUES(?1,?2) ON CONFLICT(token) DO UPDATE SET peer=excluded.peer",params![id,peer.to_string()])?;
        }
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(Reserved {
            request,
            token: id.into(),
            generation,
            binding: self.digest.clone(),
            operation,
            target: target.map(str::to_owned),
        })
    }

    /// Consume reservation and revalidate immediately before the caller dispatches.
    /// Policy still belongs to Core; no SQLite transaction survives this return.
    pub fn dispatch(&mut self, permit: Reserved) -> Result<Dispatched, Error> {
        match self.dispatch_checked(&permit) {
            Ok(()) => Ok(Dispatched(permit)),
            Err(e) => {
                let _ = self.release(permit);
                Err(e)
            }
        }
    }
    /// Listener containment: a failed/cancelled pre-dispatch reservation cannot pin slots.
    pub fn release(&mut self, permit: Reserved) -> Result<(), Error> {
        self.recheck()?;
        if permit.binding != self.digest {
            return Err(Error::Denied);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        tx.execute("UPDATE execution_audit SET outcome='unavailable',ended=?1 WHERE request=?2 AND token=?3 AND generation=?4 AND outcome='reserved'",params![now,permit.request,permit.token,permit.generation])?;
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(())
    }
    fn dispatch_checked(&mut self, permit: &Reserved) -> Result<(), Error> {
        self.recheck()?;
        if permit.binding != self.digest {
            return Err(Error::Denied);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        if permit.generation != generation(&tx)? || !reviewed(&tx)? {
            return Err(Error::Disabled);
        }
        let current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tokens WHERE id=?1 AND generation=?2 AND revoked_at IS NULL AND expires>?3)",params![permit.token,permit.generation,now],|r|r.get(0))?;
        if !current {
            return Err(Error::Denied);
        }
        let active:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM external_token_bindings WHERE token=?1) OR EXISTS(SELECT 1 FROM external_token_bindings b JOIN external_agents a ON a.key=b.endpoint WHERE b.token=?1 AND a.enabled=1)",[&permit.token],|r|r.get(0))?;
        if !active {
            return Err(Error::Denied);
        }
        if tx.execute("UPDATE execution_audit SET outcome='dispatched' WHERE request=?1 AND token=?2 AND generation=?3 AND outcome='reserved'",params![permit.request,permit.token,permit.generation])?!=1 {return Err(Error::Denied);}
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(())
    }
    /// A real result can finish after revocation/disable. Do not relabel it as
    /// non-execution. A lost result remains unknown, never automatically retried.
    pub fn finish(&mut self, permit: Dispatched, outcome: Outcome) -> Result<(), Error> {
        self.recheck()?;
        let permit = permit.0;
        if permit.binding != self.digest {
            return Err(Error::Denied);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        if tx.execute("UPDATE execution_audit SET outcome=?1,ended=?2 WHERE request=?3 AND token=?4 AND generation=?5 AND outcome='dispatched'",params![outcome.code(),now,permit.request,permit.token,permit.generation])?!=1 {return Err(Error::Denied);}
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn audit_view(&self, owner: &Owner<'_>) -> Result<serde_json::Value, Error> {
        self.owner(owner)?;
        let mut stmt=self.db.prepare("SELECT request,token,operation,target,started,ended,outcome FROM execution_audit ORDER BY started DESC,request DESC LIMIT 100")?;
        let rows=stmt.query_map([],|r|Ok(serde_json::json!({"request_id":r.get::<_,String>(0)?,"token":r.get::<_,String>(1)?,"operation":r.get::<_,String>(2)?,"target":r.get::<_,Option<String>>(3)?,"started":chrono::DateTime::<chrono::Utc>::from_timestamp(r.get(4)?,0).map(|t|t.to_rfc3339()),"ended":r.get::<_,Option<i64>>(5)?.and_then(|t|chrono::DateTime::<chrono::Utc>::from_timestamp(t,0)).map(|t|t.to_rfc3339()),"outcome":r.get::<_,String>(6)?})))?;
        Ok(
            serde_json::json!({"identity":{"workspace":self.binding.workspace,"app":self.binding.app,"agent":self.binding.agent},"requests":rows.collect::<Result<Vec<_>,_>>()?}),
        )
    }
    pub(crate) fn state(&self, owner: &Owner<'_>) -> Result<serde_json::Value, Error> {
        self.owner(owner)?;
        let (generation, enabled): (String, bool) = self.db.query_row(
            "SELECT generation,reviewed FROM settings WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(
            serde_json::json!({"generation":generation,"enabled":enabled,"tokens":self.list(owner)?}),
        )
    }
    /// Owner maintenance only: bounded storage and monotonic clock checkpoint.
    /// Future periodic caller must itself be bounded; never call for auth errors.
    pub fn maintenance(&mut self, owner: &Owner<'_>) -> Result<usize, Error> {
        self.owner(owner)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        let deleted=tx.execute("DELETE FROM execution_audit WHERE ended IS NOT NULL AND ended<?1 AND outcome IN ('completed','policy_denied','unavailable')",[now.saturating_sub(30*DAY)])?;
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(deleted)
    }
}

fn management(
    tx: &Transaction<'_>,
    owner: &str,
    action: &str,
    token: &str,
    now: i64,
) -> Result<(), Error> {
    // Independent ring: external capacity cannot prevent containment actions.
    tx.execute("DELETE FROM management_audit WHERE id IN (SELECT id FROM management_audit ORDER BY id DESC LIMIT -1 OFFSET 999)",[])?;
    tx.execute(
        "INSERT INTO management_audit(owner,action,token,at) VALUES(?1,?2,?3,?4)",
        params![owner, action, token, now],
    )?;
    Ok(())
}
fn generation(tx: &Transaction<'_>) -> Result<String, Error> {
    Ok(
        tx.query_row("SELECT generation FROM settings WHERE id=1", [], |r| {
            r.get(0)
        })?,
    )
}
fn reviewed(tx: &Transaction<'_>) -> Result<bool, Error> {
    Ok(tx.query_row("SELECT reviewed FROM settings WHERE id=1", [], |r| r.get(0))?)
}
fn sample_time(tx: &Transaction<'_>) -> Result<i64, Error> {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| Error::Invalid)?
            .as_secs(),
    )
    .map_err(|_| Error::Invalid)?;
    let high: i64 = tx.query_row("SELECT high_water FROM settings WHERE id=1", [], |r| {
        r.get(0)
    })?;
    Ok(now.max(high))
}
fn same_identity(db: &Connection, b: &Binding) -> Result<bool, Error> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT workspace,app,agent FROM identity WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(row.is_some_and(|(w, a, g)| w == b.workspace && a == b.app && g == b.agent))
}
/// Current time, never earlier than the store's clock high-water mark (read-only).
fn now_floor(db: &Connection) -> Result<i64, Error> {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| Error::Invalid)?
            .as_secs(),
    )
    .map_err(|_| Error::Invalid)?;
    let high: i64 = db.query_row("SELECT high_water FROM settings WHERE id=1", [], |r| {
        r.get(0)
    })?;
    Ok(now.max(high))
}
fn checkpoint(tx: &Transaction<'_>, now: i64) -> Result<(), Error> {
    tx.execute(
        "UPDATE settings SET high_water=max(high_water,?1) WHERE id=1",
        [now],
    )?;
    Ok(())
}
fn parse(value: &str) -> Result<(&str, &str), Error> {
    if value.len() != 102 {
        return Err(Error::Denied);
    }
    let (id, secret) = value
        .strip_prefix("ext1_")
        .and_then(|v| v.split_once('.'))
        .ok_or(Error::Denied)?;
    if !hex(id, 32) || !hex(secret, 64) {
        return Err(Error::Denied);
    }
    Ok((id, secret))
}
fn verifier(binding: &[u8], generation: &str, id: &str, secret: &str) -> Vec<u8> {
    hash(&[
        b"treer.external-agent.verifier.v1",
        binding,
        generation.as_bytes(),
        id.as_bytes(),
        secret.as_bytes(),
    ])
}
fn hash(parts: &[&[u8]]) -> Vec<u8> {
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part);
    }
    h.finalize().to_vec()
}
fn random_hex(bytes: usize) -> Result<String, Error> {
    let mut out = vec![0; bytes];
    getrandom::fill(&mut out).map_err(|_| Error::PrivateStorage)?;
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}
fn hex(v: &str, len: usize) -> bool {
    v.len() == len
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identifier(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn valid_name(v: &str) -> bool {
    !v.trim().is_empty() && v.len()<=128 && v.chars().count()<=64 && !v.chars().any(|c|c.is_control()||matches!(c,'\u{00ad}'|'\u{034f}'|'\u{fe00}'..='\u{fe0f}'|'\u{e0000}'..='\u{e01ef}'|'\u{fff9}'..='\u{fffb}'|'\u{061c}'|'\u{200b}'..='\u{200f}'|'\u{2028}'..='\u{202e}'|'\u{2060}'..='\u{206f}'|'\u{feff}'))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

fn external_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.as_bytes()[0].is_ascii_alphanumeric()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
impl Store {
    pub(crate) fn external_agents(&self, owner: &Owner<'_>) -> Result<serde_json::Value, Error> {
        self.owner(owner)?;
        let mut st = self.db.prepare(
            "SELECT key,name,enabled,created FROM external_agents ORDER BY created,key LIMIT 100",
        )?;
        let rows=st.query_map([],|r| { let key:String=r.get(0)?; Ok(serde_json::json!({"key":key,"name":r.get::<_,String>(1)?,"enabled":r.get::<_,bool>(2)?,"created":r.get::<_,i64>(3)?,"address":format!("{}/{}",self.binding.agent,key)})) })?.collect::<Result<Vec<_>,_>>()?;
        Ok(serde_json::json!({"agents":rows}))
    }
    pub(crate) fn register_external(
        &mut self,
        owner: &Owner<'_>,
        key: &str,
        name: &str,
        days: u16,
    ) -> Result<Created, Error> {
        if !matches!(days, 30 | 90) {
            return Err(Error::Invalid);
        }
        self.create_scoped(owner, name, i64::from(days) * DAY, Some((key, name, true)))
    }
    pub(crate) fn external_token(
        &mut self,
        owner: &Owner<'_>,
        key: &str,
        name: &str,
        days: u16,
    ) -> Result<Created, Error> {
        if !matches!(days, 30 | 90) {
            return Err(Error::Invalid);
        }
        self.create_scoped(owner, name, i64::from(days) * DAY, Some((key, name, false)))
    }
    pub(crate) fn revoke_created_external(
        &mut self,
        permit: &Dispatched,
        id: &str,
    ) -> Result<(), Error> {
        self.recheck()?;
        if permit.0.binding != self.digest
            || !matches!(permit.operation(), Operation::Registry)
            || !hex(id, 32)
        {
            return Err(Error::Denied);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = sample_time(&tx)?;
        let current = generation(&tx)?;
        let allowed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tokens t JOIN external_token_bindings b ON b.token=t.id WHERE t.id=?1 AND t.creator=?2 AND t.generation=?3) AND NOT EXISTS(SELECT 1 FROM external_token_bindings WHERE token=?2)",params![id,permit.0.token,current],|r|r.get(0))?;
        if !allowed || current != permit.0.generation {
            return Err(Error::Denied);
        }
        let changed = tx.execute(
            "UPDATE tokens SET revoked_at=?1,revoker=?2 WHERE id=?3 AND revoked_at IS NULL",
            params![now, permit.0.token, id],
        )?;
        if changed == 1 {
            management(&tx, &permit.0.token, "revoked", id, now)?;
        }
        checkpoint(&tx, now)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn scoped_token(&self, permit: &Dispatched) -> Result<bool, Error> {
        if permit.0.binding != self.digest {
            return Err(Error::Denied);
        }
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM external_token_bindings WHERE token=?1)",
            [&permit.0.token],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn external_owner<'a>(&'a self, permit: &'a Dispatched) -> Result<Owner<'a>, Error> {
        if !matches!(permit.operation(), Operation::Registry) || permit.0.binding != self.digest {
            return Err(Error::Denied);
        }
        Ok(Owner {
            binding: &self.binding,
            subject: &permit.0.token,
        })
    }
    pub(crate) fn register_external_from(
        &mut self,
        permit: &Dispatched,
        key: &str,
        name: &str,
        days: u16,
    ) -> Result<Created, Error> {
        if !matches!(permit.operation(), Operation::Registry) || permit.0.binding != self.digest {
            return Err(Error::Denied);
        }
        let binding = self.binding.clone();
        let owner = Owner {
            binding: &binding,
            subject: &permit.0.token,
        };
        self.register_external(&owner, key, name, days)
    }
}

#[path = "store_device.rs"]
mod device;
