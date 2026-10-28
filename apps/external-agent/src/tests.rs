use super::*;
use rusqlite::hooks::Action;
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn binding() -> Binding {
    Binding {
        workspace: "ws_fixture".into(),
        app: "app_fixture".into(),
        agent: "ag_fixture".into(),
    }
}
fn owner(binding: &Binding) -> Owner<'_> {
    Owner {
        binding,
        subject: "u_owner",
    }
}
fn fixture(limits: Limits) -> (tempfile::TempDir, std::path::PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("state");
    let s = Store::open(&path, binding(), limits).unwrap();
    (dir, path, s)
}
fn token(s: &mut Store) -> (String, String) {
    let b = binding();
    let c = s.create(&owner(&b), "fixture", 30).unwrap();
    let id = c.record.id.clone();
    (id, c.into_plaintext())
}
fn enable(s: &mut Store) {
    let b = binding();
    s.policy_reviewed(&owner(&b), true).unwrap();
}
fn audit(s: &Store) -> Vec<(String, String)> {
    let mut st =
        s.db.prepare("SELECT request,outcome FROM execution_audit ORDER BY request")
            .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn defaults_closed_and_owner_proof_exact_scope() {
    let (_d, _p, mut s) = fixture(Limits::default());
    let (_id, value) = token(&mut s);
    let before = s.db.total_changes();
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Disabled)
    ));
    assert!(matches!(
        s.admit("bad", Operation::Cli),
        Err(Error::Disabled)
    ));
    assert_eq!(s.db.total_changes(), before);
    let mut b = binding();
    b.workspace = "other".into();
    assert!(matches!(
        s.policy_reviewed(&owner(&b), true),
        Err(Error::Denied)
    ));
    assert!(matches!(s.list(&owner(&b)), Err(Error::Denied)));
    assert!(matches!(
        s.create(&owner(&b), "wrong", 30),
        Err(Error::Denied)
    ));
    assert_eq!(s.db.total_changes(), before);
    enable(&mut s);
    assert!(s.admit(&value, Operation::Cli).is_ok());
}

#[test]
fn verifier_only_secret_unique_and_no_debug_serialization_surface() {
    let (_d, _p, mut s) = fixture(Limits::default());
    let (id, value) = token(&mut s);
    let (id2, value2) = token(&mut s);
    assert_ne!(id, id2);
    assert_ne!(value, value2);
    let (parsed, secret) = parse(&value).unwrap();
    assert_eq!(id, parsed);
    assert_eq!(secret.len(), 64);
    let stored: Vec<u8> =
        s.db.query_row("SELECT verifier FROM tokens WHERE id=?1", [&id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored.len(), 32);
    assert_ne!(stored, secret.as_bytes());
    let tx = s.db.transaction().unwrap();
    let g = generation(&tx).unwrap();
    assert_eq!(stored, verifier(&s.digest, &g, &id, secret));
    assert_ne!(stored, verifier(&s.digest, &g, &id2, secret));
    assert_ne!(stored, verifier(&s.digest, "other", &id, secret));
    drop(tx);
    let b = binding();
    let listed = s.list(&owner(&b)).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(!format!("{listed:?}").contains(secret));
    // Neither audit table contains arbitrary request text or a credential field.
    let mut stmt=s.db.prepare("SELECT name FROM pragma_table_info('execution_audit') UNION ALL SELECT name FROM pragma_table_info('management_audit')").unwrap();
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!names.iter().any(|n| matches!(
        n.as_str(),
        "secret" | "verifier" | "body" | "prompt" | "argv"
    )));
}

#[test]
fn reject_flood_never_writes_and_cannot_disable_valid_credential() {
    let (_d, _p, mut s) = fixture(Limits::default());
    let (id, value) = token(&mut s);
    enable(&mut s);
    let wrong = format!("ext1_{id}.{}", "0".repeat(64));
    let unknown = format!("ext1_{}.{}", "f".repeat(32), "e".repeat(64));
    let before = s.db.total_changes();
    for _ in 0..1000 {
        for v in [wrong.as_str(), unknown.as_str(), "malformed"] {
            assert!(matches!(s.admit(v, Operation::Cli), Err(Error::Denied)));
        }
    }
    assert_eq!(s.db.total_changes(), before);
    assert!(audit(&s).is_empty());
    assert!(s.admit(&value, Operation::Cli).is_ok());
}

#[test]
fn revoke_before_dispatch_refuses_but_after_dispatch_preserves_real_result() {
    let (_d, _p, mut s) = fixture(Limits::default());
    let (id, value) = token(&mut s);
    enable(&mut s);
    let pending = s.admit(&value, Operation::Cli).unwrap();
    let sent = s.admit(&value, Operation::Cli).unwrap();
    let sent = s.dispatch(sent).unwrap();
    assert_eq!(sent.token_id(), id);
    assert!(hex(sent.request_id(), 32));
    let b = binding();
    s.revoke(&owner(&b), &id).unwrap();
    assert!(matches!(s.dispatch(pending), Err(Error::Denied)));
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Denied)
    ));
    s.policy_reviewed(&owner(&b), false).unwrap();
    s.finish(sent, Outcome::Completed).unwrap();
    let outcomes = audit(&s);
    assert!(outcomes.iter().any(|v| v.1 == "completed"));
    assert!(outcomes.iter().any(|v| v.1 == "unavailable"));
    assert!(!outcomes.iter().any(|v| v.1 == "reserved"));
    let changes = s.db.total_changes();
    s.revoke(&owner(&b), &id).unwrap();
    assert!(s.db.total_changes() >= changes); // checkpoint is allowed on owner actions.
    let count: i64 =
        s.db.query_row(
            "SELECT count(*) FROM management_audit WHERE action='revoked'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn restart_preserves_revocation_and_marks_lost_results_unknown_without_retry() {
    let (_d, p, mut s) = fixture(Limits::default());
    let (id, value) = token(&mut s);
    enable(&mut s);
    let pending = s.admit(&value, Operation::Cli).unwrap();
    let dispatched = s.dispatch(pending).unwrap();
    drop(dispatched);
    let b = binding();
    s.revoke(&owner(&b), &id).unwrap();
    drop(s);
    let mut s = Store::open(&p, binding(), Limits::default()).unwrap();
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Denied)
    ));
    assert_eq!(audit(&s)[0].1, "outcome_unknown");
    assert_eq!(s.maintenance(&owner(&b)).unwrap(), 0);
}

#[test]
fn limits_leave_owner_containment_available_and_token_quota_protects_another() {
    let limits = Limits {
        audit_rows: 2,
        audit_per_token: 1,
        ..Limits::default()
    };
    let (_d, _p, mut s) = fixture(limits);
    let (id, value) = token(&mut s);
    let (_, other) = token(&mut s);
    enable(&mut s);
    let pending = s.admit(&value, Operation::Cli).unwrap();
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Capacity)
    ));
    let other = s.admit(&other, Operation::Cli).unwrap();
    let dispatched = s.dispatch(other).unwrap();
    let b = binding();
    assert_eq!(s.list(&owner(&b)).unwrap().len(), 2);
    s.revoke(&owner(&b), &id).unwrap();
    s.policy_reviewed(&owner(&b), false).unwrap();
    assert!(s.dispatch(pending).is_err());
    s.finish(dispatched, Outcome::Unknown).unwrap();
    assert_eq!(audit(&s).len(), 2);
    let revoked: bool =
        s.db.query_row(
            "SELECT revoked_at IS NOT NULL AND revoker='u_owner' FROM tokens WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(revoked);
}

#[test]
fn live_and_record_caps_do_not_block_revoke_or_list() {
    let limits = Limits {
        live_tokens: 1,
        token_records: 2,
        ..Limits::default()
    };
    let (_d, _p, mut s) = fixture(limits);
    let (id, _) = token(&mut s);
    let b = binding();
    assert!(matches!(
        s.create(&owner(&b), "second", 30),
        Err(Error::Capacity)
    ));
    s.revoke(&owner(&b), &id).unwrap();
    let (second, _) = token(&mut s);
    s.revoke(&owner(&b), &second).unwrap();
    assert!(matches!(
        s.create(&owner(&b), "third", 90),
        Err(Error::Capacity)
    ));
    assert_eq!(s.list(&owner(&b)).unwrap().len(), 2);
    s.policy_reviewed(&owner(&b), false).unwrap();
}

#[test]
fn generation_rotation_invalidates_tokens_pending_permits_and_activation() {
    let (_d, p, mut s) = fixture(Limits::default());
    let (_, value) = token(&mut s);
    enable(&mut s);
    let pending = s.admit(&value, Operation::Cli).unwrap();
    let b = binding();
    s.rotate_after_restore(&owner(&b)).unwrap();
    assert!(matches!(s.dispatch(pending), Err(Error::Disabled)));
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Disabled)
    ));
    enable(&mut s);
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Denied)
    ));
    drop(s);
    let mut s = Store::open(&p, binding(), Limits::default()).unwrap();
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Denied)
    ));
}

#[test]
fn high_water_expiry_and_retention_preserve_incomplete_records() {
    let (_d, p, mut s) = fixture(Limits::default());
    let (_, value) = token(&mut s);
    enable(&mut s);
    let reserved = s.admit(&value, Operation::Cli).unwrap();
    let dispatched = s.dispatch(reserved).unwrap();
    s.finish(dispatched, Outcome::Completed).unwrap();
    s.admit(&value, Operation::Cli).unwrap(); // An incomplete row must survive retention.

    // Be clearly beyond retention, not one second after token expiry:
    // completion can cross a wall-clock second after token creation.
    let future: i64 =
        s.db.query_row("SELECT max(expires)+86400 FROM tokens", [], |r| r.get(0))
            .unwrap();
    s.db.execute("UPDATE settings SET high_water=?1", [future])
        .unwrap();
    let before = s.db.total_changes();
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Denied)
    ));
    assert_eq!(before, s.db.total_changes());
    let b = binding();
    assert_eq!(s.maintenance(&owner(&b)).unwrap(), 1);
    assert_eq!(audit(&s)[0].1, "reserved");
    drop(s);
    let mut s = Store::open(&p, binding(), Limits::default()).unwrap();
    assert!(matches!(
        s.admit(&value, Operation::Cli),
        Err(Error::Denied)
    ));
    assert_eq!(audit(&s)[0].1, "outcome_unknown");
}

#[test]
fn hostile_inputs_cannot_create_credentials_target_another_workspace_or_write_audit() {
    let (_d, _p, mut s) = fixture(Limits::default());
    let _ = token(&mut s);
    enable(&mut s);
    let b = binding();
    for name in ["", "   ", "line\nname", "x\u{202e}y", "x\0y"] {
        assert!(matches!(
            s.create(&owner(&b), name, 30),
            Err(Error::Invalid)
        ));
    }
    assert!(matches!(s.create(&owner(&b), "ok", 0), Err(Error::Invalid)));
    assert!(matches!(
        s.create(&owner(&b), "ok", 365),
        Err(Error::Invalid)
    ));
    assert!(audit(&s).is_empty());
}

#[test]
fn durable_audit_reservation_must_commit_before_any_fake_dispatch() {
    let (_d, _p, mut s) = fixture(Limits::default());
    let (_, value) = token(&mut s);
    enable(&mut s);
    // A commit refusal must roll back both last_used and the audit row.
    s.db.commit_hook(Some(|| true));
    let called = AtomicUsize::new(0);
    let reservation = s.admit(&value, Operation::Cli);
    if reservation.is_ok() {
        called.fetch_add(1, Ordering::Relaxed);
    }
    assert_eq!(called.load(Ordering::Relaxed), 0);
    assert!(reservation.is_err());
    assert!(audit(&s).is_empty());
    let used: bool =
        s.db.query_row("SELECT last_used IS NULL FROM tokens", [], |r| r.get(0))
            .unwrap();
    assert!(used);
}

#[test]
fn central_hook_rolls_back_revocation_on_drift_and_restoring_path_stays_latched() {
    let (_d, path, mut s) = fixture(Limits::default());
    let (id, value) = token(&mut s);
    enable(&mut s);
    let dbpath = path.join("external-agent.sqlite3");
    let renamed = path.join("moved.sqlite3");
    let original = dbpath.clone();
    let moved = renamed.clone();
    // Keep the REAL safe commit hook, and replace the path during UPDATE.
    s.db.update_hook(Some(move |_: Action, _: &str, table: &str, _: i64| {
        if table == "tokens" {
            fs::rename(&original, &moved).unwrap();
        }
    }));
    let b = binding();
    assert!(s.revoke(&owner(&b), &id).is_err());
    fs::rename(&renamed, &dbpath).unwrap();
    assert!(s.recheck().is_err());
    assert!(s.admit(&value, Operation::Cli).is_err());
    assert!(s.list(&owner(&b)).is_err());
    s.db.update_hook(None::<fn(Action, &str, &str, i64)>);
    let revoked: bool =
        s.db.query_row(
            "SELECT revoked_at IS NOT NULL FROM tokens WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!revoked);
    drop(s);
    let s = Store::open(&path, binding(), Limits::default()).unwrap();
    assert!(s.recheck().is_ok());
}

#[test]
fn admission_drift_rolls_back_audit_and_never_returns_dispatch_permit() {
    let (_d, path, mut s) = fixture(Limits::default());
    let (_, value) = token(&mut s);
    enable(&mut s);
    let original = path.join("external-agent.sqlite3");
    let moved = path.join("moved.sqlite3");
    let o = original.clone();
    let m = moved.clone();
    let called = Arc::new(AtomicUsize::new(0));
    let h = called.clone();
    s.db.update_hook(Some(move |_: Action, _: &str, t: &str, _: i64| {
        if t == "execution_audit" {
            h.fetch_add(1, Ordering::Relaxed);
            fs::rename(&o, &m).unwrap();
        }
    }));
    assert!(s.admit(&value, Operation::Cli).is_err());
    assert_eq!(called.load(Ordering::Relaxed), 1);
    fs::rename(&moved, &original).unwrap();
    assert!(audit(&s).is_empty());
    assert!(s.recheck().is_err());
}

#[test]
fn private_storage_permissions_busy_reopen_symlink_and_identity() {
    let (_d, p, s) = fixture(Limits::default());
    assert!(matches!(
        Store::open(&p, binding(), Limits::default()),
        Err(Error::Busy)
    ));
    assert_eq!(fs::metadata(&p).unwrap().mode() & 0o7777, 0o700);
    for name in [
        "external-agent.lock",
        "external-agent.sqlite3",
        "external-agent.sqlite3-wal",
        "external-agent.sqlite3-shm",
    ] {
        assert_eq!(fs::metadata(p.join(name)).unwrap().mode() & 0o7777, 0o600);
    }
    drop(s);
    let mut other = binding();
    other.agent = "ag_other".into();
    assert!(Store::open(&p, other, Limits::default()).is_err());
    let s = Store::open(&p, binding(), Limits::default()).unwrap();
    drop(s);
    let link = p.with_file_name("link");
    symlink(&p, &link).unwrap();
    assert!(Store::open(&link, binding(), Limits::default()).is_err());
    fs::set_permissions(
        p.join("external-agent.sqlite3"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(Store::open(&p, binding(), Limits::default()).is_err());
}

#[test]
fn sqlite_lock_survives_admission_and_metadata_rechecks() {
    let (_d, path, mut s) = fixture(Limits::default());
    let (_, value) = token(&mut s);
    enable(&mut s);
    for _ in 0..3 {
        // A separate PID is required: POSIX locks belong to the process.
        let result = std::process::Command::new("/usr/bin/python3")
            .env_clear()
            .args(["-I", "-c", "import fcntl,sys\nwith open(sys.argv[1], 'r+b') as f:\n try: fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB, 510, 0x40000002)\n except BlockingIOError: sys.exit(0)\n sys.exit(1)"])
            .arg(path.join("external-agent.sqlite3"))
            .output().unwrap();
        assert!(result.status.success(), "SQLite shared lock was lost");
        s.recheck().unwrap();
    }
    let reserved = s.admit(&value, Operation::Cli).unwrap();
    let dispatched = s.dispatch(reserved).unwrap();
    s.finish(dispatched, Outcome::Completed).unwrap();
    drop(s);
    let s = Store::open(&path, binding(), Limits::default()).unwrap();
    assert_eq!(audit(&s).len(), 1);
}

#[test]
fn reserved_release_expiry_and_completed_rolling_preserve_unknown() {
    let limits = Limits {
        audit_rows: 4,
        audit_per_token: 3,
        ..Limits::default()
    };
    let (_d, _p, mut s) = fixture(limits);
    let (_id, token) = token(&mut s);
    enable(&mut s);
    let r = s.admit(&token, Operation::Cli).unwrap();
    s.release(r).unwrap();
    assert_eq!(audit(&s)[0].1, "unavailable");
    let abandoned = s.admit(&token, Operation::Cli).unwrap();
    s.db.execute("UPDATE execution_audit SET started=started-120", [])
        .unwrap();
    let next = s.admit(&token, Operation::Cli).unwrap();
    assert!(s.dispatch(abandoned).is_err());
    let sent = s.dispatch(next).unwrap();
    s.finish(sent, Outcome::Unknown).unwrap();
    for _ in 0..8 {
        s.db.execute("UPDATE execution_audit SET started=started-120", [])
            .unwrap();
        let r = s.admit(&token, Operation::Cli).unwrap();
        let d = s.dispatch(r).unwrap();
        s.finish(d, Outcome::Completed).unwrap();
    }
    let rows = audit(&s);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows.iter().filter(|r| r.1 == "outcome_unknown").count(), 1);
}

#[test]
fn wrong_well_formed_credential_uses_no_sqlite_writer_transaction() {
    let (_d, _p, mut s) = fixture(Limits::default());
    let (_id, token) = token(&mut s);
    enable(&mut s);
    let before = s.db.total_changes();
    let r = Arc::new(AtomicUsize::new(0));
    let count = r.clone();
    s.db.authorizer(Some(move |ctx: rusqlite::hooks::AuthContext<'_>| {
        if let rusqlite::hooks::AuthAction::Transaction {
            operation: rusqlite::hooks::TransactionOperation::Begin,
        } = ctx.action
        {
            count.fetch_add(1, Ordering::Relaxed);
        }
        rusqlite::hooks::Authorization::Allow
    }));
    let mut bad = token.clone();
    let last = bad.pop().unwrap();
    bad.push(if last == '0' { '1' } else { '0' });
    for _ in 0..200 {
        assert!(matches!(s.admit(&bad, Operation::Cli), Err(Error::Denied)));
    }
    assert_eq!(r.load(Ordering::Relaxed), 0);
    assert_eq!(s.db.total_changes(), before);
    s.db.authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>);
}

#[test]
fn cli_audit_upgrade_preserves_existing_v2_tokens_generation_and_history() {
    let (_d, path, mut store) = fixture(Limits::default());
    let (_, first) = token(&mut store);
    let (_, second) = token(&mut store);
    enable(&mut store);
    let permit = store.admit(&first, Operation::Cli).unwrap();
    let permit = store.dispatch(permit).unwrap();
    store.finish(permit, Outcome::Completed).unwrap();
    let before = store.state(&owner(&binding())).unwrap();
    // Reconstruct the exact old CHECK from the maintained predecessor schema.
    store
        .db
        .execute_batch(
            "BEGIN; ALTER TABLE execution_audit RENAME TO old_audit; DROP INDEX audit_token;",
        )
        .unwrap();
    let schema = include_str!("schema.sql");
    let start = schema
        .find("CREATE TABLE IF NOT EXISTS execution_audit")
        .unwrap();
    let end = schema
        .find("CREATE TABLE IF NOT EXISTS token_sources")
        .unwrap();
    store
        .db
        .execute_batch(&schema[start..end].replace("'cli',", ""))
        .unwrap();
    // Historical rows carry retired typed-operation codes; they must survive upgrades.
    store.db.execute_batch("UPDATE old_audit SET operation='list'; INSERT INTO execution_audit SELECT * FROM old_audit; DROP TABLE old_audit; PRAGMA user_version=2; COMMIT;").unwrap();
    drop(store);
    let mut store = Store::open(&path, binding(), Limits::default()).unwrap();
    let after = store.state(&owner(&binding())).unwrap();
    assert_eq!(before["generation"], after["generation"]);
    assert_eq!(before["enabled"], after["enabled"]);
    assert_eq!(audit(&store).len(), 1);
    for value in [first, second] {
        let p = store.admit(&value, Operation::Cli).unwrap();
        let p = store.dispatch(p).unwrap();
        store.finish(p, Outcome::Completed).unwrap();
    }
    assert_eq!(audit(&store).len(), 3);
    drop(store);
    let store = Store::open(&path, binding(), Limits::default()).unwrap();
    assert_eq!(audit(&store).len(), 3);
}

#[test]
fn external_inbox_v3_upgrade_preserves_full_keys_history_and_scoped_restart() {
    let (_dir, path, mut s) = fixture(Limits::default());
    let (_, one) = token(&mut s);
    let (_, two) = token(&mut s);
    enable(&mut s);
    let p = s.admit(&one, Operation::Cli).unwrap();
    let p = s.dispatch(p).unwrap();
    s.finish(p, Outcome::Unknown).unwrap();
    let before = audit(&s);
    s.db.execute_batch(
        "DROP TABLE external_token_bindings; DROP TABLE external_agents; PRAGMA user_version=3;",
    )
    .unwrap();
    drop(s);
    let mut s = Store::open(&path, binding(), Limits::default()).unwrap();
    assert_eq!(audit(&s), before);
    for value in [&one, &two] {
        let p = s.admit(value, Operation::Cli).unwrap();
        let p = s.dispatch(p).unwrap();
        s.finish(p, Outcome::Completed).unwrap();
    }
    let b = binding();
    let c = s
        .register_external(&owner(&b), "stable", "stable", 30)
        .unwrap();
    let id = c.record.id.clone();
    let key = c.into_plaintext();
    assert!(s
        .register_external(&owner(&b), "stable", "reuse", 30)
        .is_err());
    let p = s.admit(&key, Operation::Inbox).unwrap();
    let p = s.dispatch(p).unwrap();
    assert_eq!(p.target(), Some("stable"));
    s.finish(p, Outcome::Completed).unwrap();
    drop(s);
    let mut s = Store::open(&path, binding(), Limits::default()).unwrap();
    let p = s.admit(&key, Operation::Ack).unwrap();
    let p = s.dispatch(p).unwrap();
    assert_eq!(p.target(), Some("stable"));
    s.finish(p, Outcome::Completed).unwrap();
    let p = s.admit(&key, Operation::Cli).unwrap();
    let p = s.dispatch(p).unwrap();
    s.finish(p, Outcome::Completed).unwrap();
    assert!(matches!(
        s.admit(&key, Operation::Registry),
        Err(Error::Denied)
    ));
    let p = s.admit(&key, Operation::Inbox).unwrap();
    s.revoke(&owner(&b), &id).unwrap();
    assert!(matches!(s.dispatch(p), Err(Error::Denied)));
    assert_eq!(
        s.external_agents(&owner(&b)).unwrap()["agents"][0]["address"],
        "ag_fixture/stable"
    );
}

#[test]
fn device_grant_survives_restart_claims_once_and_preserves_old_keys() {
    let (_dir, path, mut s) = fixture(Limits::default());
    let (_, old) = token(&mut s);
    enable(&mut s);
    let request = s.device_request("device1", "Device One", 600).unwrap();
    let secret = request["device_code"].as_str().unwrap().to_owned();
    let code = request["user_code"].as_str().unwrap();
    assert_eq!(
        s.device_poll(&secret).unwrap().1["status"],
        "authorization_pending"
    );
    assert_eq!(s.device_poll(&secret).unwrap().0, 429);
    let b = binding();
    assert_eq!(
        s.device_lookup(&owner(&b), code).unwrap()["request"]["name"],
        "Device One"
    );
    assert!(s.device_decide(&owner(&b), code, true, 30, false).is_err());
    s.device_decide(&owner(&b), code, true, 30, true).unwrap();
    assert!(s.device_decide(&owner(&b), code, false, 30, false).is_err());
    s.db.execute("UPDATE device_requests SET poll_after=0", [])
        .unwrap();
    drop(s);
    let mut s = Store::open(&path, binding(), Limits::default()).unwrap();
    let (status, claimed) = s.device_poll(&secret).unwrap();
    assert_eq!(status, 200);
    assert_eq!(claimed["status"], "approved");
    let key = claimed["access_token"].as_str().unwrap();
    assert_eq!(claimed["agent"]["address"], "ag_fixture/device1");
    let rows: i64 =
        s.db.query_row("SELECT count(*) FROM tokens", [], |r| r.get(0))
            .unwrap();
    for _ in 0..2 {
        assert_eq!(s.device_poll(&secret).unwrap().0, 409);
    }
    assert_eq!(
        s.db.query_row("SELECT count(*) FROM tokens", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        rows
    );
    for value in [&old, key] {
        let p = s.admit(value, Operation::Cli).unwrap();
        let p = s.dispatch(p).unwrap();
        s.finish(p, Outcome::Completed).unwrap();
    }
    let p = s.admit(key, Operation::Inbox).unwrap();
    let p = s.dispatch(p).unwrap();
    assert_eq!(p.target(), Some("device1"));
    s.finish(p, Outcome::Completed).unwrap();
    let stored: String =
        s.db.query_row(
            "SELECT device_hash||user_hash FROM device_requests",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!stored.contains(&secret));
    assert!(!stored.contains(&code.replace('-', "")));
}

#[test]
fn device_grant_deny_expiry_rate_limits_and_atomic_mint_failure() {
    let (_dir, _path, mut s) = fixture(Limits::default());
    let b = binding();
    let denied = s.device_request("denied", "Denied", 60).unwrap();
    s.device_decide(
        &owner(&b),
        denied["user_code"].as_str().unwrap(),
        false,
        30,
        false,
    )
    .unwrap();
    assert_eq!(
        s.device_poll(denied["device_code"].as_str().unwrap())
            .unwrap()
            .0,
        403
    );
    let expired = s.device_request("expired", "Expired", 60).unwrap();
    s.db.execute(
        "UPDATE device_requests SET expires=0 WHERE status='pending'",
        [],
    )
    .unwrap();
    assert_eq!(
        s.device_poll(expired["device_code"].as_str().unwrap())
            .unwrap()
            .0,
        410
    );
    let request = s.device_request("fault", "Fault", 600).unwrap();
    s.device_decide(
        &owner(&b),
        request["user_code"].as_str().unwrap(),
        true,
        30,
        true,
    )
    .unwrap();
    s.db.execute_batch("CREATE TRIGGER reject_token BEFORE INSERT ON tokens BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(s
        .device_poll(request["device_code"].as_str().unwrap())
        .is_err());
    assert_eq!(
        s.db.query_row("SELECT count(*) FROM tokens", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        s.device_lookup(&owner(&b), request["user_code"].as_str().unwrap())
            .unwrap()["request"]["status"],
        "approved"
    );
    s.db.execute_batch("DROP TRIGGER reject_token; UPDATE device_requests SET poll_after=0;")
        .unwrap();
    assert_eq!(
        s.device_poll(request["device_code"].as_str().unwrap())
            .unwrap()
            .1["status"],
        "approved"
    );
    for _ in 0..30 {
        let _ = s.device_lookup(&owner(&b), "FFFF-FFFF");
    }
    assert!(matches!(
        s.device_lookup(&owner(&b), "FFFF-FFFF"),
        Err(Error::Capacity)
    ));
    assert!(s.device_request("bad/key", "Bad", 600).is_err());
    assert!(s.device_request("fine", "Fine", 59).is_err());
}

#[test]
fn device_grants_have_bounded_pending_capacity_and_generation_scope() {
    let (_d, _p, mut s) = fixture(Limits::default());
    let first = s.device_request("agent0", "Agent", 600).unwrap();
    for n in 1..100 {
        s.device_request(&format!("agent{n}"), "Agent", 600)
            .unwrap();
    }
    assert!(matches!(
        s.device_request("overflow", "Agent", 600),
        Err(Error::Capacity)
    ));
    let b = binding();
    s.rotate_after_restore(&owner(&b)).unwrap();
    assert_eq!(
        s.device_poll(first["device_code"].as_str().unwrap())
            .unwrap()
            .0,
        410
    );
    assert!(s
        .device_lookup(&owner(&b), first["user_code"].as_str().unwrap())
        .is_err());
}

#[test]
fn device_v4_upgrade_preserves_existing_full_and_agent_keys_and_history() {
    let (_d, path, mut s) = fixture(Limits::default());
    let b = binding();
    let (_, one) = token(&mut s);
    let (_, two) = token(&mut s);
    enable(&mut s);
    let c = s
        .register_external(&owner(&b), "existing", "Existing", 30)
        .unwrap();
    let scoped = c.into_plaintext();
    let generation = s.state(&owner(&b)).unwrap()["generation"].clone();
    s.db.execute_batch(
        "DROP TABLE device_requests; DROP TABLE device_attempt_budget; PRAGMA user_version=4;",
    )
    .unwrap();
    drop(s);
    let mut s = Store::open(&path, binding(), Limits::default()).unwrap();
    assert_eq!(s.state(&owner(&b)).unwrap()["generation"], generation);
    for key in [&one, &two, &scoped] {
        let p = s.admit(key, Operation::Cli).unwrap();
        let p = s.dispatch(p).unwrap();
        s.finish(p, Outcome::Completed).unwrap();
    }
    assert_eq!(s.list(&owner(&b)).unwrap().len(), 3);
    let r = s.device_request("newdevice", "New", 600).unwrap();
    s.device_decide(&owner(&b), r["user_code"].as_str().unwrap(), true, 30, true)
        .unwrap();
    assert_eq!(
        s.device_poll(r["device_code"].as_str().unwrap()).unwrap().1["status"],
        "approved"
    );
}

#[test]
fn wrong_identity_refuses_before_any_schema_change_and_v4_upgrades() {
    fn schema(db: &Connection) -> Vec<(String, String)> {
        db.prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }
    let (_d, path, mut s) = fixture(Limits::default());
    let b = binding();
    let (_, key) = token(&mut s);
    enable(&mut s);
    let state = s.state(&owner(&b)).unwrap();
    let before = schema(&s.db);
    drop(s);
    let mut wrong = binding();
    wrong.app = "app_other".into();
    assert!(matches!(
        Store::open(&path, wrong, Limits::default()),
        Err(Error::PrivateStorage)
    ));
    let ro = Connection::open_with_flags(
        path.join("external-agent.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(schema(&ro), before);
    drop(ro);
    // A v4 store (before device grants and identity) upgrades in place.
    let s = Store::open(&path, binding(), Limits::default()).unwrap();
    s.db.execute_batch(
        "DROP TABLE device_requests; DROP TABLE device_attempt_budget; DROP TABLE identity; PRAGMA user_version=4;",
    )
    .unwrap();
    drop(s);
    let mut s = Store::open(&path, binding(), Limits::default()).unwrap();
    assert_eq!(
        s.state(&owner(&b)).unwrap()["generation"],
        state["generation"]
    );
    let p = s.admit(&key, Operation::Cli).unwrap();
    let p = s.dispatch(p).unwrap();
    s.finish(p, Outcome::Completed).unwrap();
    assert_eq!(
        s.db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
}

#[test]
fn pre_v6_store_adopts_identity_and_keeps_existing_tokens() {
    let (_d, p, mut s) = fixture(Limits::default());
    let (_, value) = token(&mut s);
    enable(&mut s);
    // Simulate a store written by the v5 release: no identity row, older user_version.
    s.db.execute_batch("DROP TABLE identity; PRAGMA user_version=5;")
        .unwrap();
    drop(s);
    let mut s = Store::open(&p, binding(), Limits::default()).unwrap();
    let version: i64 =
        s.db.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
    assert_eq!(version, 6);
    assert!(s.admit(&value, Operation::Cli).is_ok());
    drop(s);
    let mut other = binding();
    other.workspace = "ws_other".into();
    assert!(Store::open(&p, other, Limits::default()).is_err());
}
