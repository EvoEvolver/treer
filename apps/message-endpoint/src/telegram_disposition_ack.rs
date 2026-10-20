//! Exact confirmed-ACK marking for an already committed mismatch disposition.
//! Not wired to receipt completion or a dispatcher. Preparing does not grant
//! permission to ACK an arbitrary receipt: only an exact retained disposition
//! can create PendingAck. Never hold a Hub mutex/SQLite transaction during confirm.
use crate::{
    core::{AuthenticatedDelivery, Core},
    hub::Error,
    store::Store,
    telegram_disposition_identity::Identity,
    telegram_dispositions::SCHEMA_VERSION,
    telegram_receipts::Expected,
};
use rusqlite::{
    params, DatabaseName, OptionalExtension, Transaction, TransactionBehavior, TransactionState,
};

/// Non-serializable, not Clone/Debug; borrows the exact immutable delivery whose
/// receipt identity matched a committed ledger row. No caller-supplied ack flag.
pub struct PendingAck<'a> {
    authenticated: &'a AuthenticatedDelivery,
    identity: Identity,
    ordinal: i64,
    hub: String,
    workspace: String,
}

/// Only Core's exact acknowledged/already-acknowledged reply can construct this.
/// Dropping it or failing its local commit cannot promote an unconfirmed row.
pub struct ConfirmedAck<'a>(PendingAck<'a>);

/// Snapshot the committed row and its monotonic ordinal before the network ACK.
/// An intervening prune/reinsert cannot transfer confirmation to a new row.
/// The future composed caller must supply durable expected handoff fields.
pub fn prepare<'a>(
    store: &mut Store,
    authenticated: &'a AuthenticatedDelivery,
    job: i64,
    expected: Expected<'_>,
) -> Result<PendingAck<'a>, Error> {
    store.recheck_private_storage()?;
    let tx = store.db.transaction()?;
    let (hub, workspace): (String, String) = tx.query_row(
        "SELECT agent,workspace FROM hub_identity WHERE singleton=1 AND blocked=0",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let identity = Identity::from_authenticated(authenticated, &workspace, &hub, job, expected)?;
    let ordinal = validate(&tx, &identity, &hub, &workspace)?;
    tx.commit()?;
    Ok(PendingAck {
        authenticated,
        identity,
        ordinal,
        hub,
        workspace,
    })
}

impl<'a> PendingAck<'a> {
    /// Performs only the pinned Core call; no Store/connection is retained here.
    /// A missing exact delivery ID, malformed reply, timeout or CLI error cannot
    /// yield confirmation. Existing Core::ack supplies the deterministic key.
    pub fn confirm(self, core: &Core) -> Result<ConfirmedAck<'a>, Error> {
        if !core.is_identity(&self.hub) {
            return Err(Error::Binding);
        }
        core.ack(&self.authenticated.delivery().delivery_id)?;
        Ok(ConfirmedAck(self))
    }
}

impl ConfirmedAck<'_> {
    /// Short independent commit after confirmed ACK. On failure retain the row;
    /// retry by preparing again and confirming the same deterministic Core ACK.
    /// This does not clear holds, prune rows, count a mismatch or change a job.
    pub fn mark(self, store: &mut Store) -> Result<(), Error> {
        store.recheck_private_storage()?;
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.transaction_state(Some(DatabaseName::Main))? != TransactionState::Write {
            return Err(Error::Binding);
        }
        let pending = self.0;
        let ordinal = validate(&tx, &pending.identity, &pending.hub, &pending.workspace)?;
        if ordinal != pending.ordinal {
            return Err(Error::Binding);
        }
        let (key, digest) = pending.identity.storage_hashes();
        if tx.execute(
            "UPDATE telegram_terminal_dispositions SET acked=1
             WHERE ordinal=?1 AND delivery_key=?2 AND receipt_digest=?3 AND job=?4 AND conflict=0",
            params![
                ordinal,
                key.as_slice(),
                digest.as_slice(),
                pending.identity.parent().0
            ],
        )? != 1
        {
            return Err(Error::Binding);
        }
        tx.commit()?;
        Ok(())
    }
}

fn validate(
    tx: &Transaction<'_>,
    identity: &Identity,
    hub: &str,
    workspace: &str,
) -> Result<i64, Error> {
    let valid_schema: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM telegram_terminal_disposition_schema
         WHERE singleton=1 AND version=?1)",
        [SCHEMA_VERSION],
        |r| r.get(0),
    )?;
    let valid_hub: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_identity
         WHERE singleton=1 AND agent=?1 AND workspace=?2 AND blocked=0)",
        params![hub, workspace],
        |r| r.get(0),
    )?;
    if !valid_schema || !valid_hub {
        return Err(Error::Binding);
    }
    let (job, endpoint, generation, recipient) = identity.parent();
    let (key, digest) = identity.storage_hashes();
    tx.query_row(
        "SELECT d.ordinal FROM telegram_terminal_dispositions d
         JOIN inbound i ON i.id=d.job
         JOIN endpoints e ON e.id=i.endpoint AND e.generation=i.generation AND e.recipient=i.recipient
         JOIN hub_bindings b ON b.endpoint=e.id AND b.reason!='deleted'
         JOIN telegram_handoff_modes m ON m.job=i.id AND m.mode='receipt'
         WHERE d.delivery_key=?1 AND d.receipt_digest=?2 AND d.job=?3 AND d.conflict=0
          AND i.endpoint=?4 AND i.generation=?5 AND i.recipient=?6
          AND i.pending=0 AND i.core_id IS NOT NULL AND i.core_id!=?7
          AND NOT EXISTS(SELECT 1 FROM telegram_terminal_holds h WHERE h.endpoint=e.id)",
        params![key.as_slice(),digest.as_slice(),job,endpoint,generation,recipient,identity.asserted_final()],
        |r| r.get(0),
    ).optional()?.ok_or(Error::Binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{Delivery, Message, Principal},
        protocol::SenderKind,
        store::Limits,
        telegram_disposition_admission::{record_in_transaction, Admission},
        telegram_receipts::Scope,
    };
    use serde_json::json;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    fn scope() -> Scope {
        Scope {
            account_id: "bot_test".into(),
            chat_id: -100,
            topic_id: None,
            endpoint_id: "ag_endpoint".into(),
            generation: 1,
            recipient_id: "ag_recipient".into(),
        }
    }
    fn expected(scope: &Scope) -> Expected<'_> {
        Expected {
            scope,
            job_id: "job_test",
            payload_digest: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            native_message_id: 2,
        }
    }
    fn delivery() -> AuthenticatedDelivery {
        AuthenticatedDelivery::fixture(Delivery {
            delivery_id: "del_test".into(),
            recipient: Principal { kind: SenderKind::Agent, id: "app_hub".into() },
            message: Message { message_id: "msg_receipt".into(),
                sender: Principal {kind: SenderKind::Agent,id: "ag_endpoint".into()},
                body: json!({"protocol":crate::telegram_receipts::CAPABILITY,"verification":"app_asserted",
                    "scope":scope(),"job_id":"job_test","payload_digest":"a".repeat(64),
                    "native_message_id":2,"final_core_message_id":"msg_other"}).to_string(),
                context_ids: vec![] },
        }, "ws_test")
    }
    fn setup(mut store: Store) -> Store {
        crate::telegram_dispositions::initialize(&mut store).unwrap();
        store.db.execute_batch("CREATE TABLE hub_identity(singleton INTEGER PRIMARY KEY,agent TEXT,workspace TEXT,blocked INTEGER);
          INSERT INTO hub_identity VALUES(1,'app_hub','ws_test',0);
          CREATE TABLE hub_bindings(endpoint TEXT PRIMARY KEY,reason TEXT);
          INSERT INTO hub_bindings VALUES('ag_endpoint','active');
          CREATE TABLE telegram_handoff_modes(job INTEGER PRIMARY KEY,mode TEXT);
          INSERT INTO telegram_handoff_modes VALUES(1,'receipt');
          INSERT INTO endpoints(id,generation,recipient,paused) VALUES('ag_endpoint',1,'ag_recipient',0);
          INSERT INTO inbound(id,endpoint,generation,external_id,recipient,body,digest,pending,core_id)
           VALUES(1,'ag_endpoint',1,'external_test','ag_recipient','synthetic',zeroblob(32),0,'msg_final');").unwrap();
        let auth = delivery();
        let identity =
            Identity::from_authenticated(&auth, "ws_test", "app_hub", 1, expected(&scope()))
                .unwrap();
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            record_in_transaction(&tx, &identity, 1).unwrap(),
            Admission::Inserted
        );
        tx.commit().unwrap();
        store
    }
    fn memory() -> Store {
        setup(Store::memory(Limits::default()).unwrap())
    }
    fn acked(store: &Store) -> i64 {
        store
            .db
            .query_row(
                "SELECT acked FROM telegram_terminal_dispositions",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn pending<'a>(store: &mut Store, auth: &'a AuthenticatedDelivery) -> PendingAck<'a> {
        prepare(store, auth, 1, expected(&scope())).unwrap()
    }
    fn fake(reply: &str, pause: bool) -> (tempfile::TempDir, Core, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("fake-core");
        let key = crate::protocol::digest_id("ack", "app_hub", 1, "del_test");
        fs::write(
            &executable,
            format!(
                r#"#!/usr/bin/python3
import sys,time,json
from pathlib import Path
p=Path(__file__)
assert sys.argv[1:]==['message','ack','--operation-id=ep-ack-{key}','--','del_test']
with p.with_suffix('.calls').open('a') as f: f.write(json.dumps(sys.argv[1:])+'\n')
p.with_suffix('.entered').touch()
if {pause}:
 deadline=time.monotonic()+4
 while not p.with_suffix('.released').exists():
  assert time.monotonic()<deadline
  time.sleep(0.002)
if {reply:?}=='exit': sys.exit(2)
if {reply:?}=='no-reply': sys.exit(0)
print({reply:?})
"#,
                pause = if pause { "True" } else { "False" }
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let core = Core::new(&executable, "app_hub").unwrap();
        (dir, core, executable)
    }
    fn assert_calls(exe: &std::path::Path, count: usize) {
        let recorded = fs::read_to_string(exe.with_extension("calls")).unwrap();
        let calls: Vec<Vec<String>> = recorded
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(calls.len(), count);
        let key = crate::protocol::digest_id("ack", "app_hub", 1, "del_test");
        let expected = vec![
            "message".to_string(),
            "ack".into(),
            format!("--operation-id=ep-ack-{key}"),
            "--".into(),
            "del_test".into(),
        ];
        for call in calls {
            assert_eq!(call, expected);
        }
    }
    fn reply(already: bool) -> String {
        json!({"acknowledged_delivery_ids":if already{vec![]}else{vec!["del_test"]},
            "already_acknowledged_delivery_ids":if already{vec!["del_test"]}else{vec![]}})
        .to_string()
    }

    #[test]
    fn exact_ack_and_already_ack_survive_reopen_and_repeat_without_changing_parent() {
        for already in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().canonicalize().unwrap().join("private");
            let mut store = setup(Store::open(&path, Limits::default()).unwrap());
            let auth = delivery();
            let (_fake, core, exe) = fake(&reply(already), false);
            // The proof owns no Store, so both prepare and the network call are
            // over before we reopen the entire private Store for local marking.
            let confirmed = pending(&mut store, &auth).confirm(&core).unwrap();
            drop(store);
            let mut store = Store::open(&path, Limits::default()).unwrap();
            assert_eq!(acked(&store), 0);
            // A normal safety pause does not invalidate a genuine ACK.
            store
                .db
                .execute("UPDATE endpoints SET paused=1", [])
                .unwrap();
            confirmed.mark(&mut store).unwrap();
            assert_eq!(acked(&store), 1);
            pending(&mut store, &auth)
                .confirm(&core)
                .unwrap()
                .mark(&mut store)
                .unwrap();
            assert_eq!(acked(&store), 1);
            assert_calls(&exe, 2);
            let parent: (i64, String) = store
                .db
                .query_row("SELECT pending,core_id FROM inbound", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(parent, (0, "msg_final".into()));
        }
    }
    #[test]
    fn wrong_id_missing_lists_malformed_reply_and_wrong_core_never_confirm() {
        for response in [json!({"acknowledged_delivery_ids":["del_other"],"already_acknowledged_delivery_ids":[]}).to_string(),
            json!({"acknowledged_delivery_ids":[],"already_acknowledged_delivery_ids":["del_other"]}).to_string(),
            json!({"acknowledged_delivery_ids":[],"already_acknowledged_delivery_ids":[]}).to_string(),"{}".into(),"not-json".into(),"exit".into(),"no-reply".into()] {
            let mut store = memory();
            let auth = delivery();
            let (_dir,core,exe) = fake(&response,false);
            assert!(pending(&mut store,&auth).confirm(&core).is_err());
            assert_calls(&exe,1);
            assert_eq!(acked(&store),0);
        }
        let mut store = memory();
        let auth = delivery();
        let (_dir, _, exe) = fake(&reply(false), false);
        let other = Core::new(&exe, "app_other").unwrap();
        assert!(pending(&mut store, &auth).confirm(&other).is_err());
        assert!(!exe.with_extension("calls").exists());
        assert_eq!(acked(&store), 0);
    }
    #[test]
    fn unretained_wrong_payload_job_and_workspace_never_prepare() {
        let mut store = memory();
        for field in [
            "delivery",
            "message",
            "body",
            "workspace",
            "recipient",
            "sender",
        ] {
            let mut raw = delivery();
            // Reconstruct a fixture; production authenticated values remain
            // immutable, cannot be deserialized or modified by callers.
            let mut wire: serde_json::Value =
                serde_json::from_str(&raw.delivery().message.body).unwrap();
            if field == "body" {
                wire["final_core_message_id"] = json!("msg_changed");
            }
            raw = AuthenticatedDelivery::fixture(
                Delivery {
                    delivery_id: if field == "delivery" {
                        "del_other"
                    } else {
                        "del_test"
                    }
                    .into(),
                    recipient: Principal {
                        kind: SenderKind::Agent,
                        id: if field == "recipient" {
                            "app_other"
                        } else {
                            "app_hub"
                        }
                        .into(),
                    },
                    message: Message {
                        message_id: if field == "message" {
                            "msg_changed"
                        } else {
                            "msg_receipt"
                        }
                        .into(),
                        sender: Principal {
                            kind: SenderKind::Agent,
                            id: if field == "sender" {
                                "ag_other"
                            } else {
                                "ag_endpoint"
                            }
                            .into(),
                        },
                        body: wire.to_string(),
                        context_ids: vec![],
                    },
                },
                if field == "workspace" {
                    "ws_other"
                } else {
                    "ws_test"
                },
            );
            assert!(
                prepare(&mut store, &raw, 1, expected(&scope())).is_err(),
                "{field}"
            );
        }
        assert!(prepare(&mut store, &delivery(), 2, expected(&scope())).is_err());
        assert_eq!(acked(&store), 0);
    }
    #[test]
    fn durable_scope_conflict_hold_schema_and_replacement_refuse_prepare_or_mark() {
        for sql in [
            "UPDATE telegram_terminal_disposition_schema SET version=3",
            "UPDATE hub_identity SET workspace='ws_other'",
            "UPDATE hub_identity SET agent='app_other'",
            "UPDATE hub_identity SET blocked=1",
            "UPDATE telegram_handoff_modes SET mode='legacy'",
            "UPDATE endpoints SET generation=2",
            "UPDATE endpoints SET recipient='ag_other'",
            "UPDATE hub_bindings SET reason='deleted'",
            "UPDATE inbound SET pending=1",
            "UPDATE telegram_terminal_dispositions SET conflict=1",
            "INSERT INTO telegram_terminal_holds(endpoint,conflict,full) VALUES('ag_endpoint',1,0)",
            "INSERT INTO telegram_terminal_holds(endpoint,conflict,full) VALUES('ag_endpoint',0,1)",
            // Explicit fault injection, not a production cleanup path. Even
            // identical hashes and parent on a replacement row must not inherit
            // an old in-flight confirmation: the monotonic ordinal differs.
            "CREATE TEMP TABLE replacement AS SELECT * FROM telegram_terminal_dispositions; DELETE FROM telegram_terminal_dispositions; INSERT INTO telegram_terminal_dispositions(delivery_key,receipt_digest,job,first_seen) SELECT delivery_key,receipt_digest,job,first_seen FROM replacement",
        ] {
            let mut store = memory();
            let auth = delivery();
            let (_dir,core,_) = fake(&reply(false),false);
            let proof = pending(&mut store,&auth).confirm(&core).unwrap();
            store.db.execute_batch(sql).unwrap();
            assert!(proof.mark(&mut store).is_err(),"{sql}");
            assert_eq!(acked(&store),0);
            if !sql.starts_with("CREATE TEMP") { assert!(prepare(&mut store,&auth,1,expected(&scope())).is_err(),"{sql}"); }
        }
    }
    #[test]
    fn failed_marker_commit_or_dropped_confirmation_keeps_unacked_and_can_reconfirm() {
        let mut store = memory();
        let auth = delivery();
        let (_dir, core, exe) = fake(&reply(true), false);
        drop(pending(&mut store, &auth).confirm(&core).unwrap());
        assert_eq!(acked(&store), 0);
        let proof = pending(&mut store, &auth).confirm(&core).unwrap();
        store.db.commit_hook(Some(|| true));
        assert!(proof.mark(&mut store).is_err());
        store.db.commit_hook(None::<fn() -> bool>);
        assert_eq!(acked(&store), 0);
        pending(&mut store, &auth)
            .confirm(&core)
            .unwrap()
            .mark(&mut store)
            .unwrap();
        assert_eq!(acked(&store), 1);
        assert_calls(&exe, 3);
    }
    #[test]
    fn concurrent_conflict_commits_during_network_wait_and_refuses_late_marker() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("private");
        let mut store = setup(Store::open(&path, Limits::default()).unwrap());
        // Open before any pending transactions and keep this connection alive
        // until all activity ends (same-process POSIX descriptor-lock rule).
        let mut second = rusqlite::Connection::open(path.join("hub.sqlite3")).unwrap();
        let auth = delivery();
        let pending = pending(&mut store, &auth);
        let (_dir, core, exe) = fake(&reply(false), true);
        let confirmed = std::thread::scope(|scope| {
            let worker = scope.spawn(|| pending.confirm(&core));
            let deadline = Instant::now() + Duration::from_secs(3);
            while !exe.with_extension("entered").exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            let tx = second
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute("UPDATE telegram_terminal_dispositions SET conflict=1", [])
                .unwrap();
            tx.commit().unwrap();
            fs::write(exe.with_extension("released"), b"").unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !worker.is_finished() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            worker.join().unwrap().unwrap()
        });
        assert!(confirmed.mark(&mut store).is_err());
        assert_eq!(acked(&store), 0);
        assert_calls(&exe, 1);
    }
    #[test]
    fn real_private_commit_hook_rolls_back_marker_then_reopen_reconfirms_same_ack() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("private");
        let mut store = setup(Store::open(&path, Limits::default()).unwrap());
        let auth = delivery();
        let (_dir, core, exe) = fake(&reply(true), false);
        let proof = pending(&mut store, &auth).confirm(&core).unwrap();
        let database = path.join("hub.sqlite3");
        let original = path.join("original.sqlite3");
        let moved = Arc::new(AtomicBool::new(false));
        let moved_in_hook = moved.clone();
        let db_in_hook = database.clone();
        let original_in_hook = original.clone();
        // Drift inside the actual marker UPDATE, after entry recheck. Keep
        // Store::open's real #217 commit hook installed throughout.
        store.db.update_hook(Some(
            move |_: rusqlite::hooks::Action, _: &str, table: &str, _: i64| {
                if table == "telegram_terminal_dispositions"
                    && !moved_in_hook.swap(true, Ordering::SeqCst)
                {
                    fs::rename(&db_in_hook, &original_in_hook).unwrap();
                }
            },
        ));
        assert!(proof.mark(&mut store).is_err());
        assert!(moved.load(Ordering::SeqCst));
        assert_eq!(acked(&store), 0);
        fs::rename(&original, &database).unwrap();
        assert!(
            store.recheck_private_storage().is_err(),
            "drift refusal must remain sticky"
        );
        drop(store);
        let mut store = Store::open(&path, Limits::default()).unwrap();
        assert_eq!(acked(&store), 0);
        pending(&mut store, &auth)
            .confirm(&core)
            .unwrap()
            .mark(&mut store)
            .unwrap();
        assert_eq!(acked(&store), 1);
        assert_calls(&exe, 2);
    }
    #[test]
    fn ignored_marker_update_is_not_reported_as_success() {
        let mut store = memory();
        let auth = delivery();
        let (_dir, core, exe) = fake(&reply(false), false);
        let proof = pending(&mut store, &auth).confirm(&core).unwrap();
        store.db.execute_batch("CREATE TRIGGER ignore_marker BEFORE UPDATE OF acked ON telegram_terminal_dispositions BEGIN SELECT RAISE(IGNORE); END;").unwrap();
        assert!(proof.mark(&mut store).is_err());
        assert_eq!(acked(&store), 0);
        assert_calls(&exe, 1);
    }
}
