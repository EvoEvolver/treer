//! Offline storage primitives for future real-email dispatch. No network caller,
//! activation or owner-reconciliation authority is supplied by this module.
use super::{capacity, valid_time, Error, Intent, Mode, Queue};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

const PER_BINDING: i64 = 32;
const GLOBAL: i64 = 640;
// Core token mint (15s) + HTTP request (20s) + scheduling margin (10s).
const MIN_RESERVATION_LEASE: i64 = 45;

fn wall_time() -> Result<i64, Error> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .ok_or(Error::Invalid)
}

#[cfg(test)]
#[test]
fn reservation_budget_covers_actual_transport_timeouts_and_margin() {
    let required = crate::core::CLI_TIMEOUT
        + super::proxy::REQUEST_TIMEOUT
        + std::time::Duration::from_secs(10);
    assert!(std::time::Duration::from_secs(MIN_RESERVATION_LEASE as u64) >= required);
}

pub(super) fn schema(db: &Connection) -> Result<(), Error> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS email_dispatch(
        id TEXT PRIMARY KEY REFERENCES email_intents(id),
        endpoint TEXT NOT NULL, generation INTEGER NOT NULL,
        payload BLOB NOT NULL CHECK(length(payload)<=4096),
        state TEXT NOT NULL CHECK(state IN ('prepared','dispatching','uncertain','provider_accepted'))
    ); CREATE INDEX IF NOT EXISTS email_dispatch_capacity ON email_dispatch(state,endpoint);")?;
    Ok(())
}
pub(super) fn exists(db: &Connection, id: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM email_dispatch WHERE id=?1)",
        [id],
        |r| r.get(0),
    )?)
}
pub(super) fn reserved(db: &Connection, id: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM email_dispatch WHERE id=?1 AND state IN ('dispatching','uncertain'))",
        [id],
        |r| r.get(0),
    )?)
}
fn current(tx: &Transaction<'_>, intent: &Intent, now: i64) -> Result<(), Error> {
    let valid: bool = tx.query_row("SELECT EXISTS(
        SELECT 1 FROM email_intents i JOIN endpoints e ON e.id=i.endpoint AND e.generation=i.generation
        JOIN email_targets t ON t.endpoint=e.id AND t.generation=e.generation
        WHERE i.id=?1 AND i.nonce=?2 AND i.state=0 AND i.mode IS NULL AND i.lease_until>?3
        AND e.paused=0 AND t.revoked=0 AND t.address=?4 AND i.endpoint=?5 AND i.generation=?6 AND i.thread=?7)",
        params![intent.id,intent.nonce,now,intent.address,intent.endpoint,intent.generation,intent.thread], |r| r.get(0))?;
    if valid {
        Ok(())
    } else {
        Err(Error::Lease)
    }
}
/// Opaque attempt returned only after the dispatching reservation is committed.
/// No Clone/Debug/Deserialize: callers cannot construct it from request JSON or
/// accidentally put its private frozen notification/key in a diagnostic.
pub struct Attempt {
    id: String,
    nonce: String,
    endpoint: String,
    generation: i64,
    payload: Vec<u8>,
}
impl Attempt {
    /// Exact JSON bytes to use for the eventual POST, never a regenerated body.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}
#[derive(Clone, Copy)]
pub enum Outcome {
    ProviderAccepted,
    Uncertain,
}
/// Internal masked status for a future authenticated management surface. Counts
/// include all generations of this stable binding; no bodies, addresses or keys.
#[derive(serde::Serialize)]
pub struct DispatchStatus {
    pub prepared: i64,
    pub abandoned: i64,
    pub dispatching: i64,
    pub uncertain: i64,
    pub provider_accepted: i64,
    pub at_capacity: bool,
}
fn frozen_payload(intent: &Intent, url: &str) -> Result<Vec<u8>, Error> {
    let mut notification = intent.notification(url, Mode::Proxy)?;
    let marker = Sha256::digest(
        [
            b"treer-email-marker-v1\0".as_slice(),
            notification.idempotency_key.as_bytes(),
        ]
        .concat(),
    );
    let reference = marker[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    notification
        .text
        .push_str(&format!("\n\nReference: {reference}"));
    let payload = serde_json::to_vec(&notification).map_err(|_| Error::Invalid)?;
    if payload.len() > 4096 {
        return Err(Error::Invalid);
    }
    Ok(payload)
}
impl Queue<'_> {
    /// Runtime fence: sample time after acquiring the write lock, independently
    /// of reservation time. Call only after token mint; every error denies POST.
    pub fn proxy_dispatch_allowed_now(&mut self, intent: &Intent, url: &str) -> bool {
        self.proxy_dispatchable_with_clock(intent, url, wall_time)
            .unwrap_or(false)
    }
    /// Deterministic fixture adapter; production must use the fresh-clock entry.
    #[cfg(test)]
    fn proxy_dispatch_allowed(&mut self, intent: &Intent, url: &str, now: i64) -> bool {
        self.proxy_dispatchable(intent, url, now).unwrap_or(false)
    }
    /// Positive local journal fence for the post-token-mint callback. This is
    /// not Core/member/allowlist/activation authorization and must use the
    /// caller's correct Hub Store. No CLI or network while this transaction is
    /// open; no atomicity with a later external request is claimed.
    #[cfg(test)]
    fn proxy_dispatchable(&mut self, intent: &Intent, url: &str, now: i64) -> Result<bool, Error> {
        self.proxy_dispatchable_with_clock(intent, url, || Ok(now))
    }

    fn proxy_dispatchable_with_clock(
        &mut self,
        intent: &Intent,
        url: &str,
        clock: impl FnOnce() -> Result<i64, Error>,
    ) -> Result<bool, Error> {
        let expected = frozen_payload(intent, url)?;
        self.store
            .recheck_private_storage()
            .map_err(|_| Error::PrivateStorage)?;
        let tx = self
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = clock()?;
        if !valid_time(now) {
            return Err(Error::Invalid);
        }
        let valid = match current(&tx, intent, now) {
            Ok(()) => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM email_dispatch WHERE id=?1 AND endpoint=?2 AND generation=?3 AND state='dispatching' AND payload=?4)",
                params![intent.id,intent.endpoint,intent.generation,expected], |r| r.get(0))?,
            Err(Error::Lease) => false,
            Err(error) => return Err(error),
        };
        tx.commit()?;
        // A read-only transaction may not invoke SQLite's COMMIT hook.
        self.store
            .recheck_private_storage()
            .map_err(|_| Error::PrivateStorage)?;
        Ok(valid)
    }
    /// No public API is added here. HTTP callers still need the existing
    /// management authentication; global pressure is exposed as a bool only.
    pub fn proxy_delivery_status(&self, endpoint: &str) -> Result<DispatchStatus, Error> {
        if !crate::store::valid_id(endpoint) {
            return Err(Error::Invalid);
        }
        self.store
            .recheck_private_storage()
            .map_err(|_| Error::PrivateStorage)?;
        let tx = Transaction::new_unchecked(&self.store.db, TransactionBehavior::Deferred)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM endpoints WHERE id=?1)",
            [endpoint],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Denied);
        }
        let (prepared,abandoned,dispatching,uncertain,provider_accepted): (i64,i64,i64,i64,i64) = tx.query_row(
            "SELECT coalesce(sum(d.state='prepared' AND i.mode IS NULL),0),coalesce(sum(i.state=2 AND i.mode='proxy_abandoned'),0),coalesce(sum(d.state='dispatching'),0),coalesce(sum(d.state='uncertain'),0),coalesce(sum(d.state='provider_accepted'),0) FROM email_dispatch d JOIN email_intents i ON i.id=d.id WHERE d.endpoint=?1", [endpoint], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let global: i64 = tx.query_row(
            "SELECT count(*) FROM email_dispatch WHERE state IN ('dispatching','uncertain')",
            [],
            |r| r.get(0),
        )?;
        tx.commit()?;
        self.store
            .recheck_private_storage()
            .map_err(|_| Error::PrivateStorage)?;
        Ok(DispatchStatus {
            prepared,
            abandoned,
            dispatching,
            uncertain,
            provider_accepted,
            at_capacity: dispatching + uncertain >= PER_BINDING || global >= GLOBAL,
        })
    }
    /// Caller still needs fresh membership, Core pins, allowlist and activation
    /// checks. This storage operation neither authorizes nor performs a send.
    pub fn prepare_proxy_delivery(
        &mut self,
        intent: &Intent,
        url: &str,
        now: i64,
    ) -> Result<(), Error> {
        if !valid_time(now) {
            return Err(Error::Invalid);
        }
        let payload = frozen_payload(intent, url)?;
        let tx = self
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent, now)?;
        let existing: Option<Vec<u8>> = tx
            .query_row(
                "SELECT payload FROM email_dispatch WHERE id=?1",
                [&intent.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != payload {
                return Err(Error::Conflict);
            }
        } else {
            capacity(&tx, self.store.limits, &intent.endpoint, payload.len())?;
            tx.execute("INSERT INTO email_dispatch(id,endpoint,generation,payload,state) VALUES(?1,?2,?3,?4,'prepared')",
                params![intent.id,intent.endpoint,intent.generation,payload])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Reserves capacity and freezes dispatch state BEFORE returning any bytes.
    /// Requires at least 45 seconds on the current lease under the same write
    /// transaction. Insufficient budget returns no attempt or reservation.
    /// Test-only explicit-time seam; production uses begin_proxy_delivery_now.
    /// A commit failure returns no attempt; do not POST or acknowledge work.
    #[cfg(test)]
    pub(super) fn begin_proxy_delivery(
        &mut self,
        intent: &Intent,
        url: &str,
        now: i64,
    ) -> Result<Attempt, Error> {
        self.begin_proxy_delivery_with_clock(intent, url, || Ok(now))
    }

    /// Runtime reservation entry: clock sampling occurs after BEGIN IMMEDIATE,
    /// so a write-lock wait cannot consume an unobserved part of the 45s budget.
    /// This does not authorize a send or replace the independent post-mint fence.
    pub fn begin_proxy_delivery_now(
        &mut self,
        intent: &Intent,
        url: &str,
    ) -> Result<Attempt, Error> {
        self.begin_proxy_delivery_with_clock(intent, url, wall_time)
    }

    fn begin_proxy_delivery_with_clock(
        &mut self,
        intent: &Intent,
        url: &str,
        clock: impl FnOnce() -> Result<i64, Error>,
    ) -> Result<Attempt, Error> {
        let expected = frozen_payload(intent, url)?;
        let tx = self
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = clock()?;
        if !valid_time(now) {
            return Err(Error::Invalid);
        }
        current(&tx, intent, now)?;
        let lease_until: i64 = tx.query_row(
            "SELECT lease_until FROM email_intents WHERE id=?1",
            [&intent.id],
            |r| r.get(0),
        )?;
        if lease_until.saturating_sub(now) < MIN_RESERVATION_LEASE {
            return Err(Error::Lease);
        }
        let payload: Vec<u8> = tx.query_row("SELECT payload FROM email_dispatch WHERE id=?1 AND endpoint=?2 AND generation=?3 AND state='prepared'",
            params![intent.id,intent.endpoint,intent.generation], |r|r.get(0)).optional()?.ok_or(Error::Denied)?;
        if payload != expected {
            return Err(Error::Conflict);
        }
        let (local, global): (i64,i64) = tx.query_row("SELECT coalesce(sum(endpoint=?1),0),count(*) FROM email_dispatch WHERE state IN ('dispatching','uncertain')", [&intent.endpoint], |r|Ok((r.get(0)?,r.get(1)?)))?;
        if local >= PER_BINDING || global >= GLOBAL {
            return Err(Error::Full);
        }
        tx.execute(
            "UPDATE email_dispatch SET state='dispatching' WHERE id=?1",
            [&intent.id],
        )?;
        tx.commit()?;
        Ok(Attempt {
            id: intent.id.clone(),
            nonce: intent.nonce.clone(),
            endpoint: intent.endpoint.clone(),
            generation: intent.generation,
            payload,
        })
    }
    /// Persists outcome before caller may acknowledge. Uncertainty keeps its
    /// reservation and queue row. Later local refusals cannot override it.
    pub fn finish_proxy_delivery(
        &mut self,
        attempt: Attempt,
        outcome: Outcome,
    ) -> Result<(), Error> {
        let tx = self
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = match outcome {
            Outcome::ProviderAccepted => "provider_accepted",
            Outcome::Uncertain => "uncertain",
        };
        let changed = tx.execute("UPDATE email_dispatch SET state=?2 WHERE id=?1 AND endpoint=?3 AND generation=?4 AND payload=?5 AND state='dispatching'",
            params![attempt.id,state,attempt.endpoint,attempt.generation,attempt.payload])?;
        if changed != 1 {
            return Err(Error::Denied);
        }
        if matches!(outcome, Outcome::ProviderAccepted) {
            let changed = tx.execute("UPDATE email_intents SET state=1,mode='proxy',nonce=NULL,lease_until=NULL WHERE id=?1 AND nonce=?2 AND endpoint=?3 AND generation=?4 AND state=0 AND mode IS NULL",
                params![attempt.id,attempt.nonce,attempt.endpoint,attempt.generation])?;
            if changed != 1 {
                return Err(Error::Lease);
            }
        }
        tx.commit()?;
        Ok(())
    }
    /// Startup containment only, while dispatch workers are stopped. This does
    /// not resend, release slots, infer non-send or acknowledge anything.
    pub(crate) fn recover_interrupted_proxy_deliveries(&mut self) -> Result<usize, Error> {
        Ok(self.store.db.execute(
            "UPDATE email_dispatch SET state='uncertain' WHERE state='dispatching'",
            [],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        email::{Contact, REAL_RECIPIENT},
        protocol::{Envelope, Sender, SenderKind},
        store::{Limits, Store},
    };
    pub(super) const URL: &str = "https://hub.example/";
    pub(super) fn setup(store: &mut Store) {
        store.register_paused("ag_email", 1, "ag_internal").unwrap();
        Queue::new(store)
            .unwrap()
            .bind_paused(
                "ag_email",
                1,
                Contact {
                    address: REAL_RECIPIENT.into(),
                    member_id: "user_alice".into(),
                },
            )
            .unwrap();
        store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
    }
    fn intent(q: &mut Queue<'_>, n: i64) -> Intent {
        intent_at(q, n, n * 61)
    }
    pub(super) fn intent_at(q: &mut Queue<'_>, n: i64, now: i64) -> Intent {
        q.admit(
            "ag_email",
            1,
            &Envelope::Outbound {
                delivery_id: format!("delivery_{n}"),
                message_id: format!("msg_{n}"),
                original_sender: Sender {
                    kind: SenderKind::Agent,
                    id: "ag_internal".into(),
                },
                body: "BODY_SENTINEL\r\nBcc: attacker@example.test <script>alert(1)</script>"
                    .into(),
                context_ids: vec![],
            },
            now,
        )
        .unwrap();
        q.claim_proxy("ag_email", 1, now, &format!("{n:064x}"))
            .unwrap()
            .unwrap()
    }
    fn state(q: &Queue<'_>, id: &str) -> String {
        q.store
            .db
            .query_row("SELECT state FROM email_dispatch WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn fresh_clock_errors_leave_prepared_or_dispatching_state_unchanged() {
        let mut store = Store::memory(Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        assert!(matches!(
            q.begin_proxy_delivery_with_clock(&i, URL, || Err(Error::Invalid)),
            Err(Error::Invalid)
        ));
        assert_eq!(state(&q, &i.id), "prepared");
        let _attempt = q.begin_proxy_delivery(&i, URL, 61).unwrap();
        assert!(matches!(
            q.proxy_dispatchable_with_clock(&i, URL, || Err(Error::Invalid)),
            Err(Error::Invalid)
        ));
        assert_eq!(state(&q, &i.id), "dispatching");
    }

    #[test]
    fn runtime_clock_sampling_occurs_after_actual_write_lock_wait() {
        use std::{
            sync::{
                atomic::{AtomicBool, AtomicI64, Ordering},
                mpsc,
            },
            time::{Duration, Instant},
        };
        static BUSY: AtomicBool = AtomicBool::new(false);
        static CLOCK: AtomicI64 = AtomicI64::new(61);
        static ADVANCE: AtomicI64 = AtomicI64::new(77);
        fn busy(tries: i32) -> bool {
            CLOCK.store(ADVANCE.load(Ordering::SeqCst), Ordering::SeqCst);
            BUSY.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(1));
            tries < 5000
        }
        fn contended<T>(
            q: &mut Queue<'_>,
            path: &std::path::Path,
            advance: i64,
            run: impl FnOnce(&mut Queue<'_>) -> T,
        ) -> T {
            BUSY.store(false, Ordering::SeqCst);
            CLOCK.store(61, Ordering::SeqCst);
            ADVANCE.store(advance, Ordering::SeqCst);
            let (ready_tx, ready_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let path = path.to_owned();
            let blocker = std::thread::spawn(move || {
                let db = Connection::open(path).unwrap();
                db.execute_batch("BEGIN IMMEDIATE").unwrap();
                ready_tx.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while !BUSY.load(Ordering::SeqCst) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                db.execute_batch("COMMIT").unwrap();
                let _ = done_rx.recv_timeout(Duration::from_secs(5));
            });
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            q.store.db.busy_handler(Some(busy)).unwrap();
            let result = run(q);
            let _ = done_tx.send(());
            blocker.join().unwrap();
            q.store.db.busy_handler(None).unwrap();
            assert!(BUSY.load(Ordering::SeqCst));
            result
        }
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().canonicalize().unwrap().join("state");
        let mut store = Store::open(&directory, Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        let database = directory.join("hub.sqlite3");
        let refused = contended(&mut q, &database, 77, |q| {
            q.begin_proxy_delivery_with_clock(&i, URL, || Ok(CLOCK.load(Ordering::SeqCst)))
        });
        assert!(matches!(refused, Err(Error::Lease)));
        assert_eq!(state(&q, &i.id), "prepared");
        let _attempt = q.begin_proxy_delivery(&i, URL, 61).unwrap();
        let allowed = contended(&mut q, &database, 122, |q| {
            q.proxy_dispatchable_with_clock(&i, URL, || Ok(CLOCK.load(Ordering::SeqCst)))
        })
        .unwrap();
        assert!(!allowed);
        assert_eq!(state(&q, &i.id), "dispatching");
    }
    fn copy(a: &Attempt) -> Attempt {
        Attempt {
            id: a.id.clone(),
            nonce: a.nonce.clone(),
            endpoint: a.endpoint.clone(),
            generation: a.generation,
            payload: a.payload.clone(),
        }
    }
    #[test]
    fn reservation_requires_45_seconds_without_changing_insufficient_claims() {
        for now in [61, 76, 77, 120, 121, 122] {
            let mut store = Store::memory(Limits::default()).unwrap();
            setup(&mut store);
            let mut q = Queue::new(&mut store).unwrap();
            let i = intent(&mut q, 1); // Claim expires at 121.
            q.prepare_proxy_delivery(&i, URL, 61).unwrap();
            let before: (i64, i64, i64, String, i64) = q.store.db.query_row(
                "SELECT state,attempts,next_try,nonce,lease_until FROM email_intents WHERE id=?1",
                [&i.id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
            ).unwrap();
            let result = q.begin_proxy_delivery(&i, URL, now);
            let status = q.proxy_delivery_status("ag_email").unwrap();
            if now <= 76 {
                assert!(result.is_ok(), "45 seconds inclusive must admit");
                assert_eq!(status.dispatching, 1);
                assert_eq!(status.prepared, 0);
            } else {
                assert!(matches!(result, Err(Error::Lease)));
                assert_eq!(status.dispatching, 0);
                assert_eq!(status.uncertain, 0);
                assert_eq!(status.prepared, 1);
                assert!(!reserved(&q.store.db, &i.id).unwrap());
            }
            let after: (i64, i64, i64, String, i64) = q.store.db.query_row(
                "SELECT state,attempts,next_try,nonce,lease_until FROM email_intents WHERE id=?1",
                [&i.id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
            ).unwrap();
            assert_eq!(before, after);
            if now == 77 {
                // A fresh claim after expiry can use the same frozen intent;
                // admission refusal did not turn this into an uncertain send.
                let fresh = q
                    .claim_proxy("ag_email", 1, 122, &"e".repeat(64))
                    .unwrap()
                    .unwrap();
                assert_eq!(fresh.id, i.id);
                assert!(q.begin_proxy_delivery(&fresh, URL, 122).is_ok());
            }
        }
    }

    #[test]
    fn journal_positive_fence_rechecks_current_scope_bytes_and_lifecycle() {
        for change in [
            "pause",
            "revoke",
            "generation",
            "nonce",
            "address",
            "thread",
            "url",
            "payload",
            "expire",
            "accepted",
            "uncertain",
            "restart",
        ] {
            let mut store = Store::memory(Limits::default()).unwrap();
            setup(&mut store);
            let mut q = Queue::new(&mut store).unwrap();
            let mut i = intent(&mut q, 1);
            q.prepare_proxy_delivery(&i, URL, 61).unwrap();
            assert!(!q.proxy_dispatchable(&i, URL, 61).unwrap());
            let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
            assert!(q.proxy_dispatchable(&i, URL, 61).unwrap());
            assert!(!q.dispatchable(&i, 61).unwrap());
            let mut url = URL;
            let mut now = 61;
            match change {
                "pause" => {
                    q.store
                        .db
                        .execute("UPDATE endpoints SET paused=1", [])
                        .unwrap();
                }
                "revoke" => {
                    q.store
                        .db
                        .execute("UPDATE email_targets SET revoked=1", [])
                        .unwrap();
                }
                "generation" => {
                    q.store
                        .db
                        .execute("UPDATE endpoints SET generation=2", [])
                        .unwrap();
                }
                "nonce" => i.nonce = "f".repeat(64),
                "address" => {
                    q.store
                        .db
                        .execute(
                            "UPDATE email_targets SET address='different@example.test'",
                            [],
                        )
                        .unwrap();
                }
                "thread" => i.thread = "f".repeat(64),
                "url" => url = "https://changed.example/",
                "payload" => {
                    q.store
                        .db
                        .execute("UPDATE email_dispatch SET payload=X'7b7d'", [])
                        .unwrap();
                }
                "expire" => now = 122,
                "accepted" => q
                    .finish_proxy_delivery(a, Outcome::ProviderAccepted)
                    .unwrap(),
                "uncertain" => q.finish_proxy_delivery(a, Outcome::Uncertain).unwrap(),
                "restart" => {
                    q.recover_interrupted_proxy_deliveries().unwrap();
                }
                _ => unreachable!(),
            }
            assert!(!q.proxy_dispatchable(&i, url, now).unwrap(), "{change}");
        }
    }
    #[test]
    fn journal_fence_and_masked_status_refuse_private_path_drift() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("state");
        let mut store = Store::open(&path, Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        let _attempt = q.begin_proxy_delivery(&i, URL, 61).unwrap();
        assert!(q.proxy_dispatchable(&i, URL, 61).unwrap());
        assert!(!q.proxy_dispatch_allowed(&i, URL, -1));
        assert!(!q.proxy_dispatch_allowed(&i, "invalid-url", 61));
        assert!(q.proxy_dispatch_allowed(&i, URL, 61));
        let status = serde_json::to_value(q.proxy_delivery_status("ag_email").unwrap()).unwrap();
        assert_eq!(
            status,
            serde_json::json!({"prepared":0,"abandoned":0,"dispatching":1,"uncertain":0,"provider_accepted":0,"at_capacity":false})
        );
        assert!(q.proxy_delivery_status("ag_missing").is_err());
        let database = path.join("hub.sqlite3");
        let original = path.join("original.sqlite3");
        std::fs::rename(&database, &original).unwrap();
        assert!(q.proxy_dispatchable(&i, URL, 61).is_err());
        assert!(!q.proxy_dispatch_allowed(&i, URL, 61));
        assert!(q.proxy_delivery_status("ag_email").is_err());
        std::fs::rename(original, database).unwrap();
        assert!(q.proxy_dispatchable(&i, URL, 61).is_err());
        assert!(!q.proxy_dispatch_allowed(&i, URL, 61));
        assert!(q.proxy_delivery_status("ag_email").is_err());
    }
    #[cfg(feature = "hub")]
    pub(super) fn fake_core(path: &std::path::Path, succeeds: bool) -> crate::core::Core {
        use std::os::unix::fs::PermissionsExt;
        let exe = path.join("fake-treer");
        let source = if succeeds {
            r#"#!/usr/bin/python3
import json,sys
assert sys.argv[1:]==['token','create','svc_hub','--json']
print(json.dumps({'access_token':'synthetic.app.signature','token_type':'Bearer','audience':'svc_hub','expires_in':300}))
"#
        } else {
            "#!/bin/sh\nexit 1\n"
        };
        std::fs::write(&exe, source).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        crate::core::Core::new(&exe, "app_own").unwrap()
    }
    #[cfg(feature = "hub")]
    #[test]
    fn in_flight_http_stop_preserves_acceptance_or_contains_lost_completion_on_reopen() {
        use crate::email::proxy::Sender as ProxySender;
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::{
                atomic::{AtomicBool, AtomicUsize, Ordering},
                mpsc, Arc, Mutex,
            },
            thread,
            time::{Duration, Instant},
        };
        for lose_completion in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().canonicalize().unwrap().join("state");
            let core = fake_core(dir.path(), true);
            let mut store = Store::open(&path, Limits::default()).unwrap();
            setup(&mut store);
            let now = wall_time().unwrap();
            let (intent, attempt) = {
                let mut q = Queue::new(&mut store).unwrap();
                let i = intent_at(&mut q, 1, now);
                q.prepare_proxy_delivery(&i, URL, now).unwrap();
                let attempt = q.begin_proxy_delivery_now(&i, URL).unwrap();
                (i, attempt)
            };
            let expected = attempt.payload().to_vec();
            let payload = expected.clone();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let sender = ProxySender::new(
                &format!("http://{}/", listener.local_addr().unwrap()),
                "svc_hub",
            )
            .unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let server_requests = requests.clone();
            let (received, observed) = mpsc::channel();
            let (release, response) = mpsc::channel();
            let server = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline);
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(e) => panic!("fake proxy accept: {e}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    head.extend(byte);
                    assert!(head.len() < 16384);
                }
                let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
                assert!(head.starts_with("post /api/apps/svc_hub/email http/1.1\r\n"));
                assert!(head.contains("authorization: bearer synthetic.app.signature\r\n"));
                let size = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                assert_eq!(size, expected.len());
                let mut actual = vec![0; size];
                stream.read_exact(&mut actual).unwrap();
                assert_eq!(actual, expected);
                server_requests.fetch_add(1, Ordering::Release);
                received.send(()).unwrap(); // POST consumed; no response yet.
                response.recv_timeout(Duration::from_secs(5)).unwrap();
                let body = r#"{"status":"sent","email_id":"email_test"}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                listener
            });
            let shared = Arc::new(Mutex::new(store));
            let stopped = Arc::new(AtomicBool::new(false));
            let (worker_state, stop) = (shared.clone(), stopped.clone());
            let worker = thread::spawn(move || {
                let completion = sender.send_committed(&core, attempt, || {
                    if stop.load(Ordering::Acquire) {
                        return false;
                    }
                    let mut store = worker_state.lock().unwrap();
                    Queue::new(&mut store)
                        .unwrap()
                        .proxy_dispatch_allowed_now(&intent, URL)
                });
                (intent, completion)
            });
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(requests.load(Ordering::Acquire), 1);
            stopped.store(true, Ordering::Release);
            // The transport must not hold SQLite while waiting for its response.
            {
                let store = shared.try_lock().expect("HTTP retained the database mutex");
                let mut other = Connection::open(path.join("hub.sqlite3")).unwrap();
                other.busy_timeout(Duration::from_secs(1)).unwrap();
                let tx = other
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                tx.execute("UPDATE endpoints SET paused=1 WHERE id='ag_email'", [])
                    .unwrap();
                tx.commit().unwrap();
                assert!(store
                    .db
                    .query_row(
                        "SELECT paused FROM endpoints WHERE id='ag_email'",
                        [],
                        |r| r.get::<_, bool>(0)
                    )
                    .unwrap());
            }
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker.is_finished() || !server.is_finished() {
                assert!(
                    Instant::now() < deadline,
                    "fake HTTP threads did not finish"
                );
                thread::sleep(Duration::from_millis(2));
            }
            let (intent, completion) = worker.join().unwrap();
            assert!(stopped.load(Ordering::Acquire));
            let listener = server.join().unwrap();
            assert!(
                matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
            );
            let mut store = match Arc::try_unwrap(shared) {
                Ok(mutex) => mutex.into_inner().unwrap(),
                Err(_) => panic!("transport still owns state after join"),
            };
            {
                let mut q = Queue::new(&mut store).unwrap();
                assert_eq!(state(&q, &intent.id), "dispatching");
                if lose_completion {
                    drop(completion);
                } else {
                    completion.commit(&mut q).unwrap();
                }
            }
            drop(store);
            let reopened = Store::open(&path, Limits::default()).unwrap();
            let mut reopened =
                crate::email_workers::tests::recover_private_store_with_startup_lease(reopened);
            let mut q = Queue::new(&mut reopened).unwrap();
            assert_eq!(
                state(&q, &intent.id),
                if lose_completion {
                    "uncertain"
                } else {
                    "provider_accepted"
                }
            );
            let (done, mode, frozen): (i64, Option<String>, Vec<u8>) = q.store.db.query_row(
                "SELECT i.state,i.mode,d.payload FROM email_intents i JOIN email_dispatch d ON d.id=i.id WHERE i.id=?1", [&intent.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            ).unwrap();
            assert_eq!(frozen, payload);
            assert_eq!(done, i64::from(!lose_completion));
            assert_eq!(
                mode.as_deref(),
                if lose_completion { None } else { Some("proxy") }
            );
            // Restore ordinary dispatch eligibility: the journal, not pause,
            // must prevent another reservation after accepted/lost completion.
            q.store
                .db
                .execute("UPDATE endpoints SET paused=0 WHERE id='ag_email'", [])
                .unwrap();
            assert!(q.begin_proxy_delivery_now(&intent, URL).is_err());
            assert!(q
                .claim_proxy("ag_email", 1, now + 61, &"c".repeat(64))
                .unwrap()
                .is_none());
            let counts = q.proxy_delivery_status("ag_email").unwrap();
            assert_eq!(requests.load(Ordering::Acquire), 1);
            assert_eq!(reserved(&q.store.db, &intent.id).unwrap(), lose_completion);
            assert_eq!(counts.uncertain, i64::from(lose_completion));
            assert_eq!(counts.provider_accepted, i64::from(!lose_completion));
            // Keep the listener until after reopen/recovery and future claim.
            assert!(
                matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
            );
            assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 0);
        }
    }
    #[cfg(feature = "hub")]
    #[test]
    fn committed_http_posts_frozen_bytes_and_never_retries_ambiguity() {
        use crate::email::proxy::Sender;
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
            time::Duration,
        };
        const ACCEPTED: &str = "HTTP/1.1 200 OK\r\nContent-Length: 41\r\nConnection: close\r\n\r\n{\"status\":\"sent\",\"email_id\":\"email_test\"}";
        for (reply, fail_commit) in [
            (ACCEPTED, false),
            (ACCEPTED, true),
            ("", false), // Connection lost after the whole payload was consumed.
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}",
                false,
            ),
            (
                "HTTP/1.1 503 Test\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                false,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let core = fake_core(dir.path(), true);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let sender = Sender::new(
                &format!("http://{}/", listener.local_addr().unwrap()),
                "svc_hub",
            )
            .unwrap();
            let mut store = Store::memory(Limits::default()).unwrap();
            setup(&mut store);
            let mut q = Queue::new(&mut store).unwrap();
            let i = intent(&mut q, 1);
            q.prepare_proxy_delivery(&i, URL, 61).unwrap();
            let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
            let expected = a.payload().to_vec();
            let join = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    head.extend(byte);
                    assert!(head.len() < 16384);
                }
                let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
                assert!(head.starts_with("post /api/apps/svc_hub/email http/1.1\r\n"));
                assert!(head.contains("content-type: application/json\r\n"));
                assert!(head.contains("authorization: bearer synthetic.app.signature\r\n"));
                let size = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                assert_eq!(size, expected.len());
                let mut actual = vec![0; size];
                stream.read_exact(&mut actual).unwrap();
                assert_eq!(actual, expected);
                let _ = stream.write_all(reply.as_bytes());
                listener
            });
            let completed = sender.send_committed(&core, a, || true);
            // HTTP acceptance alone did not complete the durable intent.
            assert_eq!(state(&q, &i.id), "dispatching");
            if fail_commit {
                // In-memory test only: never replace a persistent store's #217 hook.
                q.store.db.commit_hook(Some(|| true));
                assert!(completed.commit(&mut q).is_err());
                q.store.db.commit_hook(None::<fn() -> bool>);
                assert_eq!(state(&q, &i.id), "dispatching");
                assert!(q.begin_proxy_delivery(&i, URL, 61).is_err());
                assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 1);
                assert_eq!(state(&q, &i.id), "uncertain");
                let pending: bool = q
                    .store
                    .db
                    .query_row(
                        "SELECT state=0 AND mode IS NULL FROM email_intents WHERE id=?1",
                        [&i.id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert!(pending);
                assert!(reserved(&q.store.db, &i.id).unwrap());
            } else {
                completed.commit(&mut q).unwrap();
                assert_eq!(
                    state(&q, &i.id),
                    if reply == ACCEPTED {
                        "provider_accepted"
                    } else {
                        "uncertain"
                    }
                );
            }
            assert!(q.begin_proxy_delivery(&i, URL, 61).is_err());
            assert!(q.begin_proxy_delivery(&i, URL, 100000).is_err());
            assert!(q
                .claim_proxy("ag_email", 1, 100000, &"e".repeat(64))
                .unwrap()
                .is_none());
            let listener = join.join().unwrap();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
    #[cfg(feature = "hub")]
    #[test]
    fn post_mint_journal_fence_observes_pause_or_fresh_clock_expiry_during_fake_cli() {
        use crate::{core::Core, email::proxy::Sender};
        use std::{net::TcpListener, os::unix::fs::PermissionsExt};
        for expiry in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().canonicalize().unwrap().join("state");
            let mut store = Store::open(&path, Limits::default()).unwrap();
            setup(&mut store);
            let mut q = Queue::new(&mut store).unwrap();
            let i = intent(&mut q, 1);
            q.prepare_proxy_delivery(&i, URL, 61).unwrap();
            let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
            assert!(q.proxy_dispatchable(&i, URL, 61).unwrap());
            let database = path.join("hub.sqlite3");
            let clock = temp.path().canonicalize().unwrap().join("clock");
            let exe = temp.path().join("fake-treer");
            let expire = if expiry { "True" } else { "False" };
            std::fs::write(&exe, format!(r#"#!/usr/bin/python3
import json,sqlite3,sys
from pathlib import Path
assert sys.argv[1:]==['token','create','svc_hub','--json']
db=sqlite3.connect({database:?},timeout=1)
if {expire}: Path({clock:?}).write_text('122')
else:
    db.execute('UPDATE endpoints SET paused=1')
    Path({clock:?}).write_text('61')
db.commit()
db.close()
print(json.dumps({{'access_token':'synthetic.app.signature','token_type':'Bearer','audience':'svc_hub','expires_in':300}}))
"#)).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
            let core = Core::new(&exe, "app_own").unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let sender = Sender::new(
                &format!("http://{}/", listener.local_addr().unwrap()),
                "svc_hub",
            )
            .unwrap();
            sender
                .send_committed(&core, a, || {
                    // The fake CLI publishes time only during token mint. A captured
                    // pre-mint 61 would wrongly authorize the expired lease.
                    let now: i64 = std::fs::read_to_string(&clock).unwrap().parse().unwrap();
                    q.proxy_dispatch_allowed(&i, URL, now)
                })
                .commit(&mut q)
                .unwrap();
            assert_eq!(state(&q, &i.id), "uncertain");
            assert_eq!(q.proxy_delivery_status("ag_email").unwrap().uncertain, 1);
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
    #[cfg(feature = "hub")]
    #[test]
    fn committed_dispatch_pre_post_refusal_stays_contained_and_sends_nothing() {
        use crate::email::proxy::Sender;
        use std::{cell::Cell, net::TcpListener};
        for token_ok in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let core = fake_core(dir.path(), token_ok);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let sender = Sender::new(
                &format!("http://{}/", listener.local_addr().unwrap()),
                "svc_hub",
            )
            .unwrap();
            let mut store = Store::memory(Limits::default()).unwrap();
            setup(&mut store);
            let mut q = Queue::new(&mut store).unwrap();
            let i = intent(&mut q, 1);
            q.prepare_proxy_delivery(&i, URL, 61).unwrap();
            let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
            let fenced = Cell::new(false);
            sender
                .send_committed(&core, a, || {
                    fenced.set(true);
                    false
                })
                .commit(&mut q)
                .unwrap();
            assert_eq!(fenced.get(), token_ok);
            assert_eq!(state(&q, &i.id), "uncertain");
            assert!(reserved(&q.store.db, &i.id).unwrap());
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }
    #[test]
    fn frozen_payload_conflicts_and_outbox_conversion_is_refused() {
        let mut store = Store::memory(Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        assert!(!q.dispatchable(&i, 61).unwrap());
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        assert!(matches!(
            q.prepare_proxy_delivery(&i, "https://changed.example/", 61),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            q.begin_proxy_delivery(&i, "https://changed.example/", 61),
            Err(Error::Conflict)
        ));
        let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
        let bytes = String::from_utf8(a.payload().to_vec()).unwrap();
        assert!(!bytes.contains("BODY_SENTINEL"));
        assert!(!bytes.contains("attacker@example.test"));
        let payload: serde_json::Value = serde_json::from_slice(a.payload()).unwrap();
        assert_eq!(payload["idempotency_key"], i.id);
        let text = payload["text"].as_str().unwrap();
        let reference = text.rsplit("Reference: ").next().unwrap();
        assert_eq!(reference.len(), 16);
        assert!(reference
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert!(q.complete(&i, Mode::Outbox).is_err());
        assert!(q.complete(&i, Mode::Proxy).is_err());
        q.finish_proxy_delivery(a, Outcome::ProviderAccepted)
            .unwrap();
        assert_eq!(state(&q, &i.id), "provider_accepted");
        let mode: String = q
            .store
            .db
            .query_row("SELECT mode FROM email_intents WHERE id=?1", [&i.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(mode, "proxy");
        assert!(!reserved(&q.store.db, &i.id).unwrap());
        assert!(q.begin_proxy_delivery(&i, URL, 61).is_err());
        let j = intent(&mut q, 2);
        q.complete(&j, Mode::Outbox).unwrap();
        assert!(q.prepare_proxy_delivery(&j, URL, 122).is_err());
    }
    #[test]
    fn uncertainty_is_sticky_across_time_local_failure_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("state");
        let mut store = Store::open(&path, Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
        let late = copy(&a);
        let bytes = a.payload().to_vec();
        q.finish_proxy_delivery(a, Outcome::Uncertain).unwrap();
        assert!(q
            .finish_proxy_delivery(late, Outcome::ProviderAccepted)
            .is_err());
        assert!(q.failed(&i, 122, false).is_err());
        assert!(q.failed(&i, 122, true).is_err());
        assert!(q
            .claim("ag_email", 1, 1000000, &"f".repeat(64))
            .unwrap()
            .is_none());
        assert_eq!(state(&q, &i.id), "uncertain");
        drop(store);
        let mut store = Store::open(&path, Limits::default()).unwrap();
        let mut q = Queue::new(&mut store).unwrap();
        assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 0);
        assert!(q.begin_proxy_delivery(&i, URL, 1000001).is_err());
        assert!(q
            .claim("ag_email", 1, 1000001, &"e".repeat(64))
            .unwrap()
            .is_none());
        let retained: Vec<u8> = q
            .store
            .db
            .query_row(
                "SELECT payload FROM email_dispatch WHERE id=?1",
                [&i.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bytes, retained);
        assert!(reserved(&q.store.db, &i.id).unwrap());
    }
    #[test]
    fn interrupted_attempt_is_not_reissued_and_prepared_bytes_survive_reclaim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("state");
        let mut store = Store::open(&path, Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        let frozen: Vec<u8> = q
            .store
            .db
            .query_row(
                "SELECT payload FROM email_dispatch WHERE id=?1",
                [&i.id],
                |r| r.get(0),
            )
            .unwrap();
        drop(store);
        let mut store = Store::open(&path, Limits::default()).unwrap();
        let mut q = Queue::new(&mut store).unwrap();
        assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 0);
        assert!(q
            .claim("ag_email", 1, 123, &"b".repeat(64))
            .unwrap()
            .is_none());
        let reclaimed = q
            .claim_proxy("ag_email", 1, 123, &"c".repeat(64))
            .unwrap()
            .unwrap();
        assert_eq!(reclaimed.id, i.id);
        let a = q.begin_proxy_delivery(&reclaimed, URL, 123).unwrap();
        assert_eq!(a.payload(), frozen);
        // Crash after returning an attempt, either before POST or after remote
        // acceptance. Both must retain the same unknown result on startup.
        drop(a);
        drop(store);
        let mut store = Store::open(&path, Limits::default()).unwrap();
        let mut q = Queue::new(&mut store).unwrap();
        assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 1);
        assert_eq!(state(&q, &i.id), "uncertain");
        assert!(q
            .claim("ag_email", 1, 1000000, &"d".repeat(64))
            .unwrap()
            .is_none());
        assert!(q.begin_proxy_delivery(&reclaimed, URL, 1000000).is_err());
    }
    #[test]
    fn commit_failure_never_returns_attempt_or_acknowledgeable_success() {
        let mut store = Store::memory(Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        // Test-only in-memory DB: never replace the real private-store hook.
        q.store.db.commit_hook(Some(|| true));
        assert!(q.begin_proxy_delivery(&i, URL, 61).is_err());
        q.store.db.commit_hook(None::<fn() -> bool>);
        assert_eq!(state(&q, &i.id), "prepared");
        let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
        q.store.db.commit_hook(Some(|| true));
        assert!(q
            .finish_proxy_delivery(a, Outcome::ProviderAccepted)
            .is_err());
        q.store.db.commit_hook(None::<fn() -> bool>);
        assert_eq!(state(&q, &i.id), "dispatching");
        let pending: i64 = q
            .store
            .db
            .query_row(
                "SELECT state FROM email_intents WHERE id=?1",
                [&i.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0);
        assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 1);
        assert_eq!(state(&q, &i.id), "uncertain");
    }
    #[test]
    fn reservation_cap_survives_expiry_and_does_not_overflow() {
        let mut store = Store::memory(Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        for n in 1..=32 {
            let i = intent(&mut q, n);
            q.prepare_proxy_delivery(&i, URL, n * 61).unwrap();
            let a = q.begin_proxy_delivery(&i, URL, n * 61).unwrap();
            q.finish_proxy_delivery(a, Outcome::Uncertain).unwrap();
        }
        let i = intent(&mut q, 33);
        q.prepare_proxy_delivery(&i, URL, 33 * 61).unwrap();
        assert!(matches!(
            q.begin_proxy_delivery(&i, URL, 33 * 61),
            Err(Error::Full)
        ));
        assert_eq!(state(&q, &i.id), "prepared");
        let count: i64 = q
            .store
            .db
            .query_row(
                "SELECT count(*) FROM email_dispatch WHERE state='uncertain'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 32);
        let status = q.proxy_delivery_status("ag_email").unwrap();
        assert_eq!(status.prepared, 1);
        assert_eq!(status.uncertain, 32);
        assert!(status.at_capacity);
    }
    #[test]
    fn global_reservations_apply_when_this_binding_has_no_reservations() {
        let mut store = Store::memory(Limits::default()).unwrap();
        setup(&mut store);
        store.register_paused("ag_other", 1, "ag_internal").unwrap();
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        // Synthetic oversized retained/imported history: exercise the global
        // guard independently of the normal 32-per-binding admission bound.
        q.store.db.execute_batch("INSERT INTO email_threads VALUES('thread_history','ag_other',1,'ag_internal','doomspec@outlook.com','user_alice');
            WITH RECURSIVE n(v) AS (VALUES(0) UNION ALL SELECT v+1 FROM n WHERE v<639)
            INSERT INTO email_intents(id,thread,endpoint,generation) SELECT 'history_'||v,'thread_history','ag_other',1 FROM n;
            INSERT INTO email_dispatch(id,endpoint,generation,payload,state)
            SELECT id,endpoint,generation,X'7b7d','uncertain' FROM email_intents WHERE endpoint='ag_other';").unwrap();
        assert!(matches!(
            q.begin_proxy_delivery(&i, URL, 61),
            Err(Error::Full)
        ));
        assert_eq!(state(&q, &i.id), "prepared");
        let count: i64 = q
            .store
            .db
            .query_row(
                "SELECT count(*) FROM email_dispatch WHERE state='uncertain'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 640);
        let status = q.proxy_delivery_status("ag_email").unwrap();
        assert_eq!(status.uncertain, 0);
        assert!(status.at_capacity); // Global pressure, without other bindings' counts.
    }
    #[test]
    fn frozen_payload_counts_towards_existing_byte_and_count_limits() {
        let mut store = Store::memory(Limits::default()).unwrap();
        setup(&mut store);
        let mut q = Queue::new(&mut store).unwrap();
        let i = intent(&mut q, 1);
        q.store.limits.global_bytes = 100;
        assert!(matches!(
            q.prepare_proxy_delivery(&i, URL, 61),
            Err(Error::Full)
        ));
        assert!(!exists(&q.store.db, &i.id).unwrap());
        q.store.limits.global_bytes = 1024 * 1024;
        q.store.limits.global_count = 2;
        q.prepare_proxy_delivery(&i, URL, 61).unwrap();
        assert!(matches!(
            q.admit(
                "ag_email",
                1,
                &Envelope::Outbound {
                    delivery_id: "del_next".into(),
                    message_id: "msg_next".into(),
                    original_sender: Sender {
                        kind: SenderKind::Agent,
                        id: "ag_internal".into()
                    },
                    body: "next".into(),
                    context_ids: vec![]
                },
                62
            ),
            Err(Error::Full)
        ));
    }
}

#[cfg(all(test, feature = "hub"))]
#[path = "email_crash_tests.rs"]
mod crash_tests;
