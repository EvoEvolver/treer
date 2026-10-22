use super::*;
use crate::{
    core::Core,
    email::{delivery::Outcome, Contact, Intent, Mode},
    hub::Binding,
    protocol::{Envelope, Sender, SenderKind},
    store::{Limits, Store},
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
const URL: &str = "https://hub.example/";
fn hub(store: Store) -> Hub<Core> {
    Hub::new(
        store,
        Core::new(std::path::Path::new("/usr/bin/false"), "app_hub").unwrap(),
        "app_hub",
        "ws_test",
    )
    .unwrap()
}
fn fixture() -> (tempfile::TempDir, PathBuf, Hub<Core>, Intent) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("state");
    let mut h = hub(Store::open(&path, Limits::default()).unwrap());
    h.add_binding(Binding {
        endpoint_id: "ag_email".into(),
        generation: 1,
        recipient: "ag_internal".into(),
        expected_path: "/team/email".into(),
        expected_cwd: "/workspace/email".into(),
        expected_server_id: "machine_fixture".into(),
    })
    .unwrap();
    let mut q = Queue::new(&mut h.store).unwrap();
    q.bind_paused(
        "ag_email",
        1,
        Contact {
            address: "doomspec@outlook.com".into(),
            member_id: "user_alice".into(),
        },
    )
    .unwrap();
    q.store
        .db
        .execute("UPDATE endpoints SET paused=0", [])
        .unwrap();
    q.admit(
        "ag_email",
        1,
        &Envelope::Outbound {
            delivery_id: "delivery_fixture".into(),
            message_id: "msg_fixture".into(),
            original_sender: Sender {
                kind: SenderKind::Agent,
                id: "ag_internal".into(),
            },
            body: "synthetic body".into(),
            context_ids: vec![],
        },
        61,
    )
    .unwrap();
    let intent = q
        .claim_proxy("ag_email", 1, 61, &"a".repeat(64))
        .unwrap()
        .unwrap();
    q.prepare_proxy_delivery(&intent, URL, 61).unwrap();
    q.store
        .db
        .execute("UPDATE endpoints SET paused=1", [])
        .unwrap();
    (dir, path, h, intent)
}
type IntentRow = (
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    String,
    Vec<u8>,
);
fn row(h: &Hub<Core>, id: &str) -> IntentRow {
    h.store.db.query_row("SELECT i.state,i.mode,i.nonce,i.lease_until,d.state,d.payload FROM email_intents i JOIN email_dispatch d ON d.id=i.id WHERE i.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap()
}
fn audit(h: &Hub<Core>) -> i64 {
    h.store
        .db
        .query_row("SELECT count(*) FROM hub_owner_audit", [], |r| r.get(0))
        .unwrap()
}
#[test]
fn abandonment_is_terminal_audited_and_survives_restart_without_reclaim() {
    let (_dir, path, mut h, i) = fixture();
    let before = row(&h, &i.id);
    h.abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
        .unwrap();
    let after = row(&h, &i.id);
    assert_eq!(after.0, 2);
    assert_eq!(after.1.as_deref(), Some("proxy_abandoned"));
    assert_eq!((after.2, after.3), (None, None));
    assert_eq!(after.4, "prepared");
    assert_eq!(after.5, before.5);
    assert_eq!(h.view("ag_email").unwrap().revision, 2);
    let a = h.owner_audit(None, 100).unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].actor_id, "user_owner");
    assert_eq!(a[0].action, "abandon_prepared_email");
    let detail: serde_json::Value = serde_json::from_str(&a[0].transfer_id).unwrap();
    assert_eq!(
        detail,
        serde_json::json!({"item":i.id,"previous_state":"prepared","new_revision":2})
    );
    let encoded = serde_json::to_string(&a).unwrap();
    for sentinel in [
        "doomspec@outlook.com",
        "Treer conversation update",
        "synthetic body",
        "synthetic.app.signature",
    ] {
        assert!(!encoded.contains(sentinel));
    }
    h.abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 63)
        .unwrap();
    assert_eq!(h.view("ag_email").unwrap().revision, 2);
    assert_eq!(audit(&h), 1);
    let mut q = Queue::new(&mut h.store).unwrap();
    let status = q.proxy_delivery_status("ag_email").unwrap();
    assert_eq!((status.prepared, status.abandoned), (0, 1));
    assert!(!status.at_capacity);
    q.store
        .db
        .execute("UPDATE endpoints SET paused=0", [])
        .unwrap();
    assert!(q.begin_proxy_delivery(&i, URL, 62).is_err());
    assert!(q.complete(&i, Mode::Outbox).is_err());
    assert!(q
        .claim_proxy("ag_email", 1, 1000, &"b".repeat(64))
        .unwrap()
        .is_none());
    drop(h);
    let mut h = hub(Store::open(&path, Limits::default()).unwrap());
    let mut q = Queue::new(&mut h.store).unwrap();
    assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 0);
    assert!(q
        .claim_proxy("ag_email", 1, 2000, &"c".repeat(64))
        .unwrap()
        .is_none());
    assert_eq!(row(&h, &i.id).1.as_deref(), Some("proxy_abandoned"));
    assert_eq!(audit(&h), 1);
    assert!(h
        .abandon_prepared_email_audited("ag_email", 2, &i.id, "user_owner", 2001)
        .is_err());
    assert_eq!(audit(&h), 1);
}
#[test]
fn wrong_scope_revision_actor_and_active_binding_are_refused() {
    let (_d, _p, mut h, i) = fixture();
    h.recovery_schema().unwrap();
    let before = row(&h, &i.id);
    for (endpoint, revision, id, actor) in [
        ("ag_wrong", 1, i.id.as_str(), "user_owner"),
        ("ag_email", 2, i.id.as_str(), "user_owner"),
        ("ag_email", 1, "unknown", "user_owner"),
        ("ag_email", 1, i.id.as_str(), ""),
    ] {
        assert!(h
            .abandon_prepared_email_audited(endpoint, revision, id, actor, 62)
            .is_err());
    }
    h.store
        .db
        .execute("UPDATE endpoints SET paused=0", [])
        .unwrap();
    assert!(h
        .abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
        .is_err());
    h.store
        .db
        .execute("UPDATE endpoints SET paused=1,generation=2", [])
        .unwrap();
    assert!(h
        .abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
        .is_err());
    assert_eq!(row(&h, &i.id), before);
    assert_eq!(audit(&h), 0);
}
#[test]
fn dispatching_uncertain_and_accepted_never_become_abandoned_or_release_capacity() {
    for outcome in [
        None,
        Some(Outcome::Uncertain),
        Some(Outcome::ProviderAccepted),
    ] {
        let (_d, _p, mut h, i) = fixture();
        h.recovery_schema().unwrap();
        let mut q = Queue::new(&mut h.store).unwrap();
        q.store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
        if let Some(outcome) = outcome {
            q.finish_proxy_delivery(a, outcome).unwrap();
        }
        q.store
            .db
            .execute("UPDATE endpoints SET paused=1", [])
            .unwrap();
        let before = row(&h, &i.id);
        assert!(h
            .abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
            .is_err());
        assert_eq!(row(&h, &i.id), before);
        assert_eq!(audit(&h), 0);
        let status = Queue::new(&mut h.store)
            .unwrap()
            .proxy_delivery_status("ag_email")
            .unwrap();
        assert_eq!(status.abandoned, 0);
        assert_eq!(
            status.dispatching + status.uncertain + status.provider_accepted,
            1
        );
    }
}
#[test]
fn audit_failure_rolls_back_intent_revision_and_lease() {
    let (_d, _p, mut h, i) = fixture();
    h.recovery_schema().unwrap();
    let before = row(&h, &i.id);
    h.store.db.execute_batch("CREATE TRIGGER reject_abandon_audit BEFORE INSERT ON hub_owner_audit BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(h
        .abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
        .is_err());
    assert_eq!(row(&h, &i.id), before);
    assert_eq!(audit(&h), 0);
    assert_eq!(h.view("ag_email").unwrap().revision, 1);
}
#[test]
fn intent_failure_rolls_back_preceding_audit() {
    let (_d, _p, mut h, i) = fixture();
    h.recovery_schema().unwrap();
    let before = row(&h, &i.id);
    h.store.db.execute_batch("CREATE TRIGGER reject_abandon_intent BEFORE UPDATE ON email_intents BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(h
        .abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
        .is_err());
    assert_eq!(row(&h, &i.id), before);
    assert_eq!(audit(&h), 0);
    assert_eq!(h.view("ag_email").unwrap().revision, 1);
}
#[test]
fn real_commit_hook_rolls_back_abandonment_and_restored_path_stays_refused() {
    let (_d, p, mut h, i) = fixture();
    h.recovery_schema().unwrap();
    let before = row(&h, &i.id);
    let original = p.join("hub.sqlite3");
    let moved = p.join("moved.sqlite3");
    let o = original.clone();
    let m = moved.clone();
    h.store.db.update_hook(Some(
        move |_: rusqlite::hooks::Action, _: &str, t: &str, _: i64| {
            if t == "email_intents" {
                fs::rename(&o, &m).unwrap();
            }
        },
    ));
    assert!(h
        .abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
        .is_err());
    fs::rename(moved, original).unwrap();
    assert_eq!(row(&h, &i.id), before);
    assert_eq!(audit(&h), 0);
    assert!(h
        .abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
        .is_err());
    drop(h);
    let mut h = hub(Store::open(&p, Limits::default()).unwrap());
    assert_eq!(row(&h, &i.id), before);
    h.abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62)
        .unwrap();
    assert_eq!(audit(&h), 1);
}

static CONTENDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn contention(attempt: i32) -> bool {
    CONTENDED.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(1));
    attempt < 2000
}
fn wait_contention() {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !CONTENDED.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < until,
            "second SQLite connection never contended"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
fn second_store(path: &std::path::Path) -> Store {
    // Test-only second real SQLite connection, not a second cooperating App.
    // Keep both Connections alive until all competing operations have finished.
    let mut s = Store::memory(Limits::default()).unwrap();
    s.db = rusqlite::Connection::open(path.join("hub.sqlite3")).unwrap();
    s.db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    s.db.busy_handler(Some(contention)).unwrap();
    s
}
#[test]
fn real_connection_contention_decides_both_abandon_reservation_orderings() {
    use std::sync::{atomic::Ordering, mpsc};
    use std::time::Duration;
    // Abandon owns the write lock first; the old claimed Intent loses even
    // after a subsequent resume. The real safe commit hook stays installed.
    let (_d, p, mut h, i) = fixture();
    let mut worker = second_store(&p);
    Queue::new(&mut worker).unwrap();
    let old = Intent {
        id: i.id.clone(),
        endpoint: i.endpoint.clone(),
        generation: i.generation,
        thread: i.thread.clone(),
        nonce: i.nonce.clone(),
        address: i.address.clone(),
    };
    let (entered, inside) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    h.store.db.update_hook(Some(
        move |_: rusqlite::hooks::Action, _: &str, t: &str, _: i64| {
            if t == "email_intents" {
                entered.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
            }
        },
    ));
    CONTENDED.store(false, Ordering::SeqCst);
    let (ready, waiting) = mpsc::channel();
    let (go, start) = mpsc::channel();
    let contender = std::thread::spawn(move || {
        let mut q = Queue::new(&mut worker).unwrap();
        ready.send(()).unwrap();
        start.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(q.begin_proxy_delivery(&old, URL, 62).is_err());
        q.store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        assert!(q.begin_proxy_delivery(&old, URL, 62).is_err());
        worker
    });
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    let owner = std::thread::spawn(move || {
        let result = h.abandon_prepared_email_audited("ag_email", 1, &i.id, "user_owner", 62);
        (h, i, result)
    });
    inside.recv_timeout(Duration::from_secs(2)).unwrap();
    go.send(()).unwrap();
    wait_contention();
    release.send(()).unwrap();
    let (h, i, result) = owner.join().unwrap();
    result.unwrap();
    let worker = contender.join().unwrap();
    assert_eq!(row(&h, &i.id).1.as_deref(), Some("proxy_abandoned"));
    drop(h);
    drop(worker);

    // Reservation owns the write lock first. A competing pause waits, then
    // abandonment sees committed dispatching and cannot free its slot.
    let (_d, p, mut h, i) = fixture();
    h.store
        .db
        .execute("UPDATE endpoints SET paused=0", [])
        .unwrap();
    let mut worker = second_store(&p);
    Queue::new(&mut worker).unwrap();
    h.store.db.busy_handler(Some(contention)).unwrap();
    let (entered, inside) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    worker.db.update_hook(Some(
        move |_: rusqlite::hooks::Action, _: &str, t: &str, _: i64| {
            if t == "email_dispatch" {
                entered.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(3)).unwrap();
            }
        },
    ));
    let id = i.id.clone();
    CONTENDED.store(false, Ordering::SeqCst);
    let reserve = std::thread::spawn(move || {
        let a = Queue::new(&mut worker)
            .unwrap()
            .begin_proxy_delivery(&i, URL, 61)
            .unwrap();
        (worker, a)
    });
    inside.recv_timeout(Duration::from_secs(2)).unwrap();
    let owner = std::thread::spawn(move || {
        h.store
            .db
            .execute("UPDATE endpoints SET paused=1", [])
            .unwrap();
        let result = h.abandon_prepared_email_audited("ag_email", 1, &id, "user_owner", 62);
        (h, id, result)
    });
    wait_contention();
    release.send(()).unwrap();
    let (mut worker, a) = reserve.join().unwrap();
    let (h, id, result) = owner.join().unwrap();
    assert!(matches!(result, Err(Error::Conflict)));
    assert_eq!(row(&h, &id).4, "dispatching");
    assert_eq!(audit(&h), 0);
    worker
        .db
        .update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>);
    Queue::new(&mut worker)
        .unwrap()
        .finish_proxy_delivery(a, Outcome::Uncertain)
        .unwrap();
    assert_eq!(row(&h, &id).4, "uncertain");
    drop(h);
    drop(worker);
}

struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn crash_barrier(root: &std::path::Path) -> ! {
    fs::write(root.join("ready"), b"synthetic abandonment crash").unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        assert!(
            std::time::Instant::now() < until,
            "parent did not kill crash fixture"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
#[test]
fn abandonment_crash_child() {
    let Some(root) = std::env::var_os("TREER_ABANDON_CRASH_ROOT") else {
        return;
    };
    let root = PathBuf::from(root).canonicalize().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("kind")).unwrap(),
        "synthetic-email-abandonment"
    );
    let stage = fs::read_to_string(root.join("stage")).unwrap();
    assert!(stage == "before" || stage == "after");
    let id = fs::read_to_string(root.join("id")).unwrap();
    let mut h = hub(Store::open(&root.join("state"), Limits::default()).unwrap());
    if stage == "before" {
        let r = root.clone();
        h.store.db.update_hook(Some(
            move |_: rusqlite::hooks::Action, _: &str, t: &str, _: i64| {
                if t == "email_intents" {
                    crash_barrier(&r)
                }
            },
        ));
    }
    h.abandon_prepared_email_audited_now("ag_email", 1, &id, "user_owner")
        .unwrap();
    assert_eq!(stage, "after");
    crash_barrier(&root)
}
#[test]
fn sigkill_before_and_after_abandonment_commit_never_leaves_half_state() {
    use std::{
        os::unix::process::ExitStatusExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    for stage in ["before", "after"] {
        let (d, path, h, i) = fixture();
        h.recovery_schema().unwrap();
        let original = row(&h, &i.id);
        let root = d.path().canonicalize().unwrap();
        fs::write(root.join("kind"), b"synthetic-email-abandonment").unwrap();
        fs::write(root.join("stage"), stage).unwrap();
        fs::write(root.join("id"), &i.id).unwrap();
        drop(h);
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .env("TREER_ABANDON_CRASH_ROOT", &root)
                .args([
                    "--exact",
                    "email::abandon::tests::abandonment_crash_child",
                    "--nocapture",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let until = Instant::now() + Duration::from_secs(8);
        while !root.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before barrier"
            );
            assert!(Instant::now() < until, "child did not reach barrier");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(
            Store::open(&path, Limits::default()),
            Err(crate::store::Error::Busy)
        ));
        let read = rusqlite::Connection::open_with_flags(
            path.join("hub.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let before_kill:(i64,Option<String>,i64,i64)=read.query_row("SELECT i.state,i.mode,b.revision,(SELECT count(*) FROM hub_owner_audit) FROM email_intents i JOIN hub_bindings b ON b.endpoint=i.endpoint WHERE i.id=?1",[&i.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        drop(read);
        if stage == "before" {
            assert_eq!(before_kill, (0, None, 1, 0));
        } else {
            assert_eq!(before_kill, (2, Some("proxy_abandoned".into()), 2, 1));
        }
        child.0.kill().unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < until, "SIGKILL child did not exit");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(status.signal(), Some(9));
        child.0.wait().unwrap();
        let mut h = hub(Store::open(&path, Limits::default()).unwrap());
        let mut q = Queue::new(&mut h.store).unwrap();
        assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 0);
        if stage == "before" {
            assert_eq!(row(&h, &i.id), original);
            assert_eq!(audit(&h), 0);
            h.abandon_prepared_email_audited_now("ag_email", 1, &i.id, "user_owner")
                .unwrap();
        } else {
            assert_eq!(row(&h, &i.id).1.as_deref(), Some("proxy_abandoned"));
        }
        h.store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        let mut q = Queue::new(&mut h.store).unwrap();
        assert!(q
            .claim_proxy("ag_email", 1, 2000, &"f".repeat(64))
            .unwrap()
            .is_none());
        assert_eq!(audit(&h), 1);
        assert_eq!(h.view("ag_email").unwrap().revision, 2);
    }
}

#[test]
fn owner_configuration_inspects_masked_journal_and_abandons_but_agents_cannot() {
    use crate::{
        configuration::{self, Command, Failure},
        owner_auth::BindingManager,
    };
    use std::sync::Mutex;
    let (_dir, _path, h, i) = fixture();
    let shared = std::sync::Arc::new(Mutex::new(h));
    let directory = Core::new(std::path::Path::new("/usr/bin/false"), "app_hub").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let owner = BindingManager::synthetic("user_owner", now + 60);
    let inspect = || Command::InspectEmailDelivery {
        endpoint_id: "ag_email".into(),
    };
    let snapshot =
        configuration::execute(&shared, &directory, "ws_test", &owner, inspect()).unwrap();
    let value = serde_json::to_value(snapshot).unwrap();
    assert_eq!(value["delivery"]["prepared"], 1);
    assert_eq!(value["items"][0]["intent_id"], i.id);
    assert_eq!(value["items"][0]["abandonable"], true);
    let text = value.to_string();
    for secret in ["synthetic body", "outlook", "idempotency_key", "Reference:"] {
        assert!(!text.contains(secret));
    }
    let agent = BindingManager::synthetic_agent("ag_untrusted", now + 60);
    assert!(matches!(
        configuration::execute(&shared, &directory, "ws_test", &agent, inspect()),
        Err(Failure::Forbidden)
    ));
    let abandon = |confirm| Command::AbandonPreparedEmail {
        endpoint_id: "ag_email".into(),
        revision: 1,
        intent_id: i.id.clone(),
        confirm,
    };
    assert!(matches!(
        configuration::execute(&shared, &directory, "ws_test", &owner, abandon(false)),
        Err(Failure::Invalid)
    ));
    assert!(matches!(
        configuration::execute(&shared, &directory, "ws_test", &agent, abandon(true)),
        Err(Failure::Forbidden)
    ));
    let result =
        configuration::execute(&shared, &directory, "ws_test", &owner, abandon(true)).unwrap();
    let v = serde_json::to_value(result).unwrap();
    assert_eq!(v["delivery"]["abandoned"], 1);
    assert_eq!(v["revision"], 2);
    assert_eq!(v["paused"], true);
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .owner_audit(None, 100)
            .unwrap()
            .iter()
            .filter(|r| r.action == "abandon_prepared_email")
            .count(),
        1
    );
}

fn uncertain_fixture() -> (tempfile::TempDir, PathBuf, Hub<Core>, Intent, String) {
    let (d, p, mut h, i) = fixture();
    h.store
        .db
        .execute("UPDATE endpoints SET paused=0", [])
        .unwrap();
    let mut q = Queue::new(&mut h.store).unwrap();
    let a = q.begin_proxy_delivery(&i, URL, 61).unwrap();
    q.finish_proxy_delivery(a, Outcome::Uncertain).unwrap();
    q.store
        .db
        .execute("UPDATE endpoints SET paused=1", [])
        .unwrap();
    let reference = super::super::delivery::reference(&row(&h, &i.id).5).unwrap();
    (d, p, h, i, reference)
}
#[test]
fn owner_observation_is_not_provider_acceptance_and_releases_only_exact_uncertainty() {
    use crate::{
        configuration::{self, Command, Failure},
        owner_auth::BindingManager,
    };
    let (_d, p, h, i, reference) = uncertain_fixture();
    let shared = std::sync::Arc::new(std::sync::Mutex::new(h));
    let core = Core::new(std::path::Path::new("/usr/bin/false"), "app_hub").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let command = |reference: String| Command::ResolveEmailObserved {
        endpoint_id: "ag_email".into(),
        revision: 1,
        intent_id: i.id.clone(),
        reference,
        confirm: true,
    };
    for denied in [
        BindingManager::synthetic_agent("ag_untrusted", now + 60),
        BindingManager::synthetic("user_owner", now - 1),
    ] {
        assert!(configuration::execute(
            &shared,
            &core,
            "ws_test",
            &denied,
            command(reference.clone())
        )
        .is_err());
    }
    let owner = BindingManager::synthetic("user_owner", now + 60);
    assert!(matches!(
        configuration::execute(
            &shared,
            &core,
            "ws_test",
            &owner,
            command("0000000000000000".into())
        ),
        Err(Failure::Conflict)
    ));
    assert_eq!(
        Queue::new(&mut shared.lock().unwrap().store)
            .unwrap()
            .proxy_delivery_status("ag_email")
            .unwrap()
            .uncertain,
        1
    );
    let result = configuration::execute(
        &shared,
        &core,
        "ws_test",
        &owner,
        command(reference.clone()),
    )
    .unwrap();
    let value = serde_json::to_value(result).unwrap();
    assert_eq!(value["delivery"]["uncertain"], 0);
    assert_eq!(value["delivery"]["provider_accepted"], 0);
    assert_eq!(value["items"][0]["state"], "owner_observed_receipt");
    assert_eq!(value["paused"], true);
    assert!(configuration::execute(&shared, &core, "ws_test", &owner, command(reference)).is_err());
    let h = std::sync::Arc::try_unwrap(shared)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap();
    assert_eq!(row(&h, &i.id).4, "uncertain"); // provider history never rewritten
    assert!(!crate::email::delivery::reserved(&h.store.db, &i.id).unwrap());
    assert_eq!(row(&h, &i.id).1.as_deref(), Some("proxy_owner_observed"));
    assert_eq!(audit(&h), 1);
    drop(h);
    let mut h = hub(Store::open(&p, Limits::default()).unwrap());
    let mut q = Queue::new(&mut h.store).unwrap();
    assert_eq!(q.recover_interrupted_proxy_deliveries().unwrap(), 0);
    assert_eq!(q.receipt("ag_email", 1, "msg_fixture").unwrap(), Some(1));
    q.store
        .db
        .execute("UPDATE endpoints SET paused=0", [])
        .unwrap();
    assert!(q
        .claim_proxy("ag_email", 1, 2000, &"f".repeat(64))
        .unwrap()
        .is_none());
}
#[test]
fn observation_audit_commit_failure_keeps_reservation_and_original_payload() {
    let (_d, _p, mut h, i, reference) = uncertain_fixture();
    h.recovery_schema().unwrap();
    let original = row(&h, &i.id);
    h.store.db.commit_hook(Some(|| true));
    assert!(h
        .resolve_email_observed_now("ag_email", 1, &i.id, &reference, "user_owner")
        .is_err());
    h.store.db.commit_hook(None::<fn() -> bool>);
    assert_eq!(row(&h, &i.id), original);
    assert_eq!(audit(&h), 0);
    assert_eq!(
        Queue::new(&mut h.store)
            .unwrap()
            .proxy_delivery_status("ag_email")
            .unwrap()
            .uncertain,
        1
    );
}
#[test]
fn terminal_payload_retirement_preserves_dedup_and_refuses_uncertainty_or_new_work() {
    let (_d, p, mut h, i, reference) = uncertain_fixture();
    assert!(h
        .purge_email_payload_now("ag_email", 1, &i.id, "user_owner")
        .is_err());
    h.resolve_email_observed_now("ag_email", 1, &i.id, &reference, "user_owner")
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // Synthetic fixture timestamp, no running transport or real mailbox.
    h.store
        .db
        .execute("UPDATE email_messages SET at=?1", [now])
        .unwrap();
    assert!(h
        .purge_email_payload_now("ag_email", 2, &i.id, "user_owner")
        .is_err());
    h.store
        .db
        .execute("UPDATE email_messages SET at=61", [])
        .unwrap();
    Queue::new(&mut h.store).unwrap();
    h.store
        .db
        .execute("UPDATE email_terminal_retention SET at=61", [])
        .unwrap();
    h.purge_email_payload_now("ag_email", 2, &i.id, "user_owner")
        .unwrap();
    assert!(row(&h, &i.id).5.is_empty());
    assert_eq!(row(&h, &i.id).4, "uncertain");
    let bodies: i64 = h
        .store
        .db
        .query_row("SELECT sum(length(body)) FROM email_messages", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(bodies, 0);
    assert_eq!(audit(&h), 2);
    assert!(h
        .purge_email_payload_now("ag_email", 2, &i.id, "user_owner")
        .is_err());
    drop(h);
    let mut h = hub(Store::open(&p, Limits::default()).unwrap());
    let mut q = Queue::new(&mut h.store).unwrap();
    assert_eq!(q.receipt("ag_email", 1, "msg_fixture").unwrap(), Some(1));
    q.store
        .db
        .execute("UPDATE endpoints SET paused=0", [])
        .unwrap();
    assert!(q
        .claim_proxy("ag_email", 1, now, &"f".repeat(64))
        .unwrap()
        .is_none());
    let outbound = |body: &str| Envelope::Outbound {
        delivery_id: "delivery_fixture".into(),
        message_id: "msg_fixture".into(),
        original_sender: Sender {
            kind: SenderKind::Agent,
            id: "ag_internal".into(),
        },
        body: body.into(),
        context_ids: vec![],
    };
    assert!(q
        .admit("ag_email", 1, &outbound("synthetic body"), now)
        .is_ok());
    assert!(matches!(
        q.admit("ag_email", 1, &outbound("changed"), now),
        Err(Error::Conflict)
    ));
    let count: i64 = q
        .store
        .db
        .query_row("SELECT count(*) FROM email_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1); // tombstone continues consuming count capacity
}
#[test]
fn retired_outbox_cleanup_is_idempotent_and_never_follows_links() {
    use std::os::unix::fs::symlink;
    let (d, _p, mut h, i) = fixture();
    let out = d.path().canonicalize().unwrap().join("outbox");
    fs::create_dir(&out).unwrap();
    fs::set_permissions(&out, fs::Permissions::from_mode(0o700)).unwrap();
    crate::email::write_outbox(&i, URL, &out).unwrap();
    h.store
        .db
        .execute(
            "UPDATE email_intents SET state=1,mode='outbox',nonce=NULL,lease_until=NULL",
            [],
        )
        .unwrap();
    Queue::new(&mut h.store).unwrap();
    h.store
        .db
        .execute("UPDATE email_terminal_retention SET at=61", [])
        .unwrap();
    h.purge_email_payload_now("ag_email", 1, &i.id, "user_owner")
        .unwrap();
    let name = out.join(format!("{}.json", i.id));
    let outside = d.path().join("outside");
    fs::write(&outside, b"keep").unwrap();
    fs::remove_file(&name).unwrap();
    symlink(&outside, &name).unwrap();
    assert!(Queue::new(&mut h.store)
        .unwrap()
        .purge_retired_outbox(&out)
        .is_err());
    assert_eq!(fs::read(&outside).unwrap(), b"keep");
    fs::remove_file(&name).unwrap();
    let mut q = Queue::new(&mut h.store).unwrap();
    assert_eq!(q.purge_retired_outbox(&out).unwrap(), 1);
    assert_eq!(q.purge_retired_outbox(&out).unwrap(), 0);
}

#[test]
fn twenty_one_plus_terminal_records_retire_in_batches_across_restart_without_losing_dedupe() {
    let (_d, p, mut h, i) = fixture();
    // 25 terminal notifications and source messages in ONE normal-sized thread.
    h.store
        .db
        .execute(
            "UPDATE email_intents SET state=1,mode='outbox',nonce=NULL,lease_until=NULL",
            [],
        )
        .unwrap();
    for n in 0..24 {
        let id = format!("{n:064x}");
        h.store
            .db
            .execute(
                "INSERT INTO email_messages VALUES(?1,?2,'synthetic retained payload',?3,61)",
                params![i.thread, format!("msg_{n}"), format!("digest_{n}")],
            )
            .unwrap();
        h.store.db.execute("INSERT INTO email_intents(id,thread,endpoint,generation,state,mode) VALUES(?1,?2,'ag_email',1,1,'outbox')",params![id,i.thread]).unwrap();
    }
    Queue::new(&mut h.store).unwrap();
    // New terminal history cannot immediately bypass the fixed retention age.
    assert!(h
        .purge_email_payload_now("ag_email", 1, &i.id, "user_owner")
        .is_err());
    h.store
        .db
        .execute("UPDATE email_terminal_retention SET at=61", [])
        .unwrap();
    h.purge_email_payload_now("ag_email", 1, &i.id, "user_owner")
        .unwrap();
    let counts = |h: &Hub<Core>| {
        h.store.db.query_row("SELECT (SELECT count(*) FROM email_messages WHERE body<>''),(SELECT count(*) FROM email_retired_intents),(SELECT count(*) FROM email_messages),(SELECT count(*) FROM email_intents)",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?))).unwrap()
    };
    assert_eq!(counts(&h), (5, 20, 25, 25));
    assert!(
        Queue::new(&mut h.store)
            .unwrap()
            .proxy_recovery_items("ag_email")
            .unwrap()
            .1
    );
    drop(h);
    let mut h = hub(Store::open(&p, Limits::default()).unwrap());
    h.purge_email_payload_now("ag_email", 2, &i.id, "user_owner")
        .unwrap();
    assert_eq!(counts(&h), (0, 25, 25, 25));
    assert_eq!(
        h.store
            .db
            .query_row("SELECT count(*) FROM email_outbox_purge", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        25
    );
    assert!(Queue::new(&mut h.store)
        .unwrap()
        .proxy_recovery_items("ag_email")
        .unwrap()
        .0
        .is_empty());
    assert!(h
        .purge_email_payload_now("ag_email", 3, &i.id, "user_owner")
        .is_err());
    assert_eq!(audit(&h), 2);
    assert_eq!(
        Queue::new(&mut h.store)
            .unwrap()
            .receipt("ag_email", 1, "msg_fixture")
            .unwrap(),
        Some(1)
    );
}

#[test]
fn payload_purge_agent_expired_owner_pending_reply_and_commit_failure_are_refused() {
    use crate::{
        configuration::{self, Command},
        owner_auth::BindingManager,
    };
    let (_d, _p, mut h, i) = fixture();
    h.store
        .db
        .execute(
            "UPDATE email_intents SET state=1,mode='outbox',nonce=NULL,lease_until=NULL",
            [],
        )
        .unwrap();
    Queue::new(&mut h.store).unwrap();
    h.store
        .db
        .execute("UPDATE email_terminal_retention SET at=61", [])
        .unwrap();
    let shared = std::sync::Arc::new(std::sync::Mutex::new(h));
    let core = Core::new(std::path::Path::new("/usr/bin/false"), "app_hub").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    for actor in [
        BindingManager::synthetic_agent("ag_untrusted", now + 60),
        BindingManager::synthetic("user_owner", now - 1),
    ] {
        let c = Command::PurgeEmailPayload {
            endpoint_id: "ag_email".into(),
            revision: 1,
            intent_id: i.id.clone(),
            confirm: true,
        };
        assert!(configuration::execute(&shared, &core, "ws_test", &actor, c).is_err());
    }
    let mut h = std::sync::Arc::try_unwrap(shared)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap();
    h.recovery_schema().unwrap();
    let original = row(&h, &i.id);
    // Commit veto rolls back audit, closed-thread flag, payload and revision.
    h.store.db.commit_hook(Some(|| true));
    assert!(h
        .purge_email_payload_now("ag_email", 1, &i.id, "user_owner")
        .is_err());
    h.store.db.commit_hook(None::<fn() -> bool>);
    assert_eq!(row(&h, &i.id), original);
    assert_eq!(audit(&h), 0);
    assert_eq!(h.view("ag_email").unwrap().revision, 1);
    assert_eq!(
        h.store
            .db
            .query_row("SELECT count(*) FROM email_purged_threads", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    h.store
        .db
        .execute("UPDATE email_intents SET lease_until=9999999999", [])
        .unwrap();
    assert!(h
        .purge_email_payload_now("ag_email", 1, &i.id, "user_owner")
        .is_err());
    h.store
        .db
        .execute("UPDATE email_intents SET lease_until=NULL", [])
        .unwrap();
    h.store.db.execute("INSERT INTO inbound(endpoint,generation,external_id,recipient,body,digest,pending) VALUES('ag_email',1,'reply_pending','ag_internal','pending private body',x'01',1)",[]).unwrap();
    let job = h.store.db.last_insert_rowid();
    h.store.db.execute("INSERT INTO email_replies VALUES('reply_pending',?1,'ag_email',1,'user_alice','pending private body','digest_pending',?2,61,'[]')",params![i.thread,job]).unwrap();
    assert!(h
        .purge_email_payload_now("ag_email", 1, &i.id, "user_owner")
        .is_err());
    assert_eq!(row(&h, &i.id), original);
}

#[test]
fn colliding_frozen_reference_never_resolves_or_releases_uncertainty() {
    let (_d, _p, mut h, i, reference) = uncertain_fixture();
    let payload = row(&h, &i.id).5;
    h.store.db.execute("INSERT INTO email_intents(id,thread,endpoint,generation,state) VALUES('collision',?1,'ag_email',1,0)",[&i.thread]).unwrap();
    h.store
        .db
        .execute(
            "INSERT INTO email_dispatch VALUES('collision','ag_email',1,?1,'uncertain')",
            [&payload],
        )
        .unwrap();
    assert!(h
        .resolve_email_observed_now("ag_email", 1, &i.id, &reference, "user_owner")
        .is_err());
    assert!(crate::email::delivery::reserved(&h.store.db, &i.id).unwrap());
    assert_eq!(
        Queue::new(&mut h.store)
            .unwrap()
            .proxy_delivery_status("ag_email")
            .unwrap()
            .uncertain,
        2
    );
}
