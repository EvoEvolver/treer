//! Offline storage primitives for future real-email dispatch. No network caller,
//! activation or owner-reconciliation authority is supplied by this module.
use super::{capacity, valid_time, Error, Intent, Mode, Queue};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

const PER_BINDING: i64 = 32;
const GLOBAL: i64 = 640;

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
    /// A commit failure returns no attempt; do not POST or acknowledge work.
    pub fn begin_proxy_delivery(
        &mut self,
        intent: &Intent,
        url: &str,
        now: i64,
    ) -> Result<Attempt, Error> {
        if !valid_time(now) {
            return Err(Error::Invalid);
        }
        let expected = frozen_payload(intent, url)?;
        let tx = self
            .store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent, now)?;
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
    pub fn recover_interrupted_proxy_deliveries(&mut self) -> Result<usize, Error> {
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
    const URL: &str = "https://hub.example/";
    fn setup(store: &mut Store) {
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
            n * 61,
        )
        .unwrap();
        q.claim_proxy("ag_email", 1, n * 61, &format!("{n:064x}"))
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
    fn copy(a: &Attempt) -> Attempt {
        Attempt {
            id: a.id.clone(),
            nonce: a.nonce.clone(),
            endpoint: a.endpoint.clone(),
            generation: a.generation,
            payload: a.payload.clone(),
        }
    }
    #[cfg(feature = "hub")]
    fn fake_core(path: &std::path::Path, succeeds: bool) -> crate::core::Core {
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
    fn committed_http_posts_frozen_bytes_and_never_retries_ambiguity() {
        use crate::email::proxy::Sender;
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
            time::Duration,
        };
        for reply in [
            "HTTP/1.1 200 OK\r\nContent-Length: 41\r\nConnection: close\r\n\r\n{\"status\":\"sent\",\"email_id\":\"email_test\"}",
            "", // Connection lost after the whole payload was consumed.
            "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}",
            "HTTP/1.1 503 Test\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let core = fake_core(dir.path(), true);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let sender = Sender::new(&format!("http://{}/", listener.local_addr().unwrap()), "svc_hub").unwrap();
            let mut store = Store::memory(Limits::default()).unwrap();
            setup(&mut store);
            let mut q = Queue::new(&mut store).unwrap();
            let i = intent(&mut q, 1);
            q.prepare_proxy_delivery(&i, URL, 61).unwrap();
            let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
            let expected = a.payload().to_vec();
            let join = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
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
                let size = head.lines().find_map(|l| l.strip_prefix("content-length: ")).unwrap().parse::<usize>().unwrap();
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
            completed.commit(&mut q).unwrap();
            let accepted = reply.starts_with("HTTP/1.1 200 OK\r\nContent-Length: 41");
            assert_eq!(state(&q, &i.id), if accepted { "provider_accepted" } else { "uncertain" });
            assert!(q.begin_proxy_delivery(&i, URL, 100000).is_err());
            assert!(q.claim_proxy("ag_email", 1, 100000, &"e".repeat(64)).unwrap().is_none());
            let listener = join.join().unwrap();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
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
