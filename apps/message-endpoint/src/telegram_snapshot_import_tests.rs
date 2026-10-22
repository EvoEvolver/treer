use super::*;
use crate::{hub::Binding, telegram::Target, telegram_cutoff::Mapping};
use std::os::unix::fs::PermissionsExt;
fn fixture() -> (tempfile::TempDir, PathBuf, Request) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = root.join("state");
    let mut hub = Hub::new(
        Store::open(&state, Limits::default()).unwrap(),
        telegram_cutoff::Offline,
        "app_hub",
        "ws_test",
    )
    .unwrap();
    let binding = Binding {
        endpoint_id: "ag_endpoint".into(),
        generation: 1,
        recipient: "ag_recipient".into(),
        expected_path: "/team/endpoint".into(),
        expected_cwd: "/fixture".into(),
        expected_server_id: "machine_test".into(),
    };
    hub.add_binding(binding.clone()).unwrap();
    let target = Target {
        account_id: "bot_fixture".into(),
        chat_id: -100,
        topic_id: None,
        allowed_senders: vec![42],
    };
    hub.bind_telegram("ag_endpoint", 1, target.clone()).unwrap();
    hub.store.db.execute_batch("CREATE TABLE telegram_bot_pins(account TEXT PRIMARY KEY,bot_id INTEGER NOT NULL);INSERT INTO telegram_bot_pins VALUES('bot_fixture',12345);").unwrap();
    drop(hub);
    let snapshot = root.join("snapshot.sqlite3");
    let db = rusqlite::Connection::open(&snapshot).unwrap();
    db.execute_batch("CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT);INSERT INTO metadata VALUES('bot_id','12345'),('last_update_id','41');CREATE TABLE poc_bindings(channel,account_id,conversation,target_agent_id,enabled);INSERT INTO poc_bindings VALUES('telegram','legacy','-100:0','ag_recipient',0);CREATE TABLE im_users(channel,account_id,user_id,enabled);INSERT INTO im_users VALUES('telegram','legacy','42',1);CREATE TABLE outbound_deliveries(status);INSERT INTO outbound_deliveries VALUES('acked');CREATE TABLE outbound_chunks(status);INSERT INTO outbound_chunks VALUES('sent');CREATE TABLE processed_updates(status);INSERT INTO processed_updates VALUES('completed');CREATE TABLE message_mappings(body);INSERT INTO message_mappings VALUES('SYNTHETIC_PRIVATE_BODY_NEVER_IMPORTED');").unwrap();
    drop(db);
    std::fs::set_permissions(&snapshot, std::fs::Permissions::from_mode(0o600)).unwrap();
    let hash = hash(&snapshot);
    let r = Request {
        state_directory: state,
        hub_id: "app_hub".into(),
        workspace_id: "ws_test".into(),
        snapshot,
        snapshot_sha256: hash,
        legacy_account: "legacy".into(),
        rollback: false,
        cutoff: Cutoff {
            inventory_sha256: "a".repeat(64),
            bot_id: 12345,
            account_id: "bot_fixture".into(),
            expected_offset: 0,
            legacy_last_update_id: 41,
            mappings: vec![Mapping {
                binding,
                revision: 2,
                target,
            }],
            history_loss_accepted: true,
            app_asserted_receipts_accepted: true,
            old_poller_stopped: true,
            senders_fenced: true,
            old_mailbox_zero_samples: [100, 103],
            legacy_outbox_drained: true,
        },
    };
    (t, root.join("request.json"), r)
}
fn hash(p: &Path) -> String {
    Sha256::digest(std::fs::read(p).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn request(path: &Path, r: &Request) {
    let value = serde_json::json!({"state_directory":r.state_directory,"hub_id":r.hub_id,"workspace_id":r.workspace_id,"snapshot":r.snapshot,"snapshot_sha256":r.snapshot_sha256,"legacy_account":r.legacy_account,"rollback":r.rollback,"cutoff":r.cutoff});
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
fn offset(r: &Request) -> i64 {
    let s = Store::open(&r.state_directory, Limits::default()).unwrap();
    s.db.query_row("SELECT offset FROM telegram_accounts", [], |r| r.get(0))
        .unwrap()
}
#[test]
fn closed_snapshot_metadata_import_repeat_reverse_preserves_paused_binding_and_history_cutoff() {
    let (_t, path, mut r) = fixture();
    request(&path, &r);
    let result = run_file(&path).unwrap();
    assert_eq!(result.next_offset, 42);
    assert!(!result.activation_authorized);
    assert_eq!(run_file(&path).unwrap().fingerprint, result.fingerprint);
    let s = Store::open(&r.state_directory, Limits::default()).unwrap();
    assert!(s
        .db
        .query_row("SELECT paused FROM endpoints", [], |r| r.get::<_, bool>(0))
        .unwrap());
    assert_eq!(
        s.db.query_row("SELECT count(*) FROM telegram_reply_mappings", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        s.db.query_row("SELECT count(*) FROM inbound", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(s);
    r.rollback = true;
    request(&path, &r);
    assert_eq!(run_file(&path).unwrap().next_offset, 0);
    r.rollback = false;
    request(&path, &r);
    assert!(run_file(&path).is_err());
    assert_eq!(offset(&r), 0);
}
#[test]
fn changed_source_mapping_sender_status_pin_and_sidecar_refuse_without_target_mutation() {
    for case in 0..12 {
        let (_t, path, mut r) = fixture();
        if case < 6 || case == 11 {
            let db = rusqlite::Connection::open(&r.snapshot).unwrap();
            db.execute_batch(match case {
                0 => "UPDATE metadata SET value='999' WHERE key='bot_id'",
                1 => "UPDATE poc_bindings SET target_agent_id='ag_foreign'",
                2 => "UPDATE im_users SET user_id='99'",
                3 => "UPDATE outbound_deliveries SET status='sent'",
                4 => "UPDATE outbound_chunks SET status='sending'",
                11 => "PRAGMA user_version=99",
                _ => "UPDATE processed_updates SET status='unknown'",
            })
            .unwrap();
            drop(db);
            r.snapshot_sha256 = hash(&r.snapshot);
        }
        match case {
            6 => r.snapshot_sha256 = "b".repeat(64),
            7 => {
                std::fs::write(
                    r.snapshot.with_file_name("snapshot.sqlite3-wal"),
                    b"not read",
                )
                .unwrap();
            }
            8 => {
                std::fs::set_permissions(&r.snapshot, std::fs::Permissions::from_mode(0o644))
                    .unwrap();
            }
            9 => {
                let alias = r.snapshot.with_file_name("alias");
                std::fs::hard_link(&r.snapshot, alias).unwrap();
            }
            10 => {
                r.cutoff.old_poller_stopped = false;
            }
            _ => {}
        }
        request(&path, &r);
        assert!(run_file(&path).is_err(), "case{case}");
        assert_eq!(offset(&r), 0);
    }
}
#[test]
fn actual_store_lock_and_sql_failure_block_import_and_reverse_refuses_traffic() {
    let (_t, path, mut r) = fixture();
    request(&path, &r);
    let s = Store::open(&r.state_directory, Limits::default()).unwrap();
    assert!(run_file(&path).is_err());
    drop(s);
    let s = Store::open(&r.state_directory, Limits::default()).unwrap();
    drop(s);
    // Install the COMMIT-boundary INSERT refusal after creating the empty schema.
    let s = Store::open(&r.state_directory, Limits::default()).unwrap();
    s.db.execute_batch("CREATE TABLE telegram_snapshot_imports(account TEXT PRIMARY KEY,fingerprint TEXT NOT NULL);CREATE TRIGGER refuse_import BEFORE INSERT ON telegram_snapshot_imports BEGIN SELECT RAISE(ABORT,'fixture');END;").unwrap();
    drop(s);
    assert!(run_file(&path).is_err());
    assert_eq!(offset(&r), 0);
    let s = Store::open(&r.state_directory, Limits::default()).unwrap();
    assert_eq!(
        s.db.query_row("SELECT count(*) FROM telegram_cutoff_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    s.db.execute_batch("DROP TRIGGER refuse_import;").unwrap();
    drop(s);
    run_file(&path).unwrap();
    let s = Store::open(&r.state_directory, Limits::default()).unwrap();
    s.db.execute("UPDATE telegram_accounts SET ignored=1", [])
        .unwrap();
    drop(s);
    r.rollback = true;
    request(&path, &r);
    assert!(run_file(&path).is_err());
    assert_eq!(offset(&r), 42);
}
