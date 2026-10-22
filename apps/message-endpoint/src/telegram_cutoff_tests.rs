use super::*;
use crate::{
    core::Core,
    store::{Limits, Store},
};
fn fixture() -> (Hub<Core>, Cutoff) {
    let mut h = Hub::new(
        Store::memory(Limits::default()).unwrap(),
        Core::new(std::path::Path::new("/usr/bin/false"), "app_hub").unwrap(),
        "app_hub",
        "ws_test",
    )
    .unwrap();
    let binding = Binding {
        endpoint_id: "ag_endpoint".into(),
        generation: 1,
        recipient: "ag_internal".into(),
        expected_path: "/team/endpoint".into(),
        expected_cwd: "/workspace/endpoint".into(),
        expected_server_id: "machine_fixture".into(),
    };
    h.add_binding(binding.clone()).unwrap();
    let target = Target {
        account_id: "bot_fixture".into(),
        chat_id: -100,
        topic_id: None,
        allowed_senders: vec![42],
    };
    h.bind_telegram("ag_endpoint", 1, target.clone()).unwrap();
    h.store.db.execute_batch("CREATE TABLE telegram_bot_pins(account TEXT PRIMARY KEY,bot_id INTEGER NOT NULL);INSERT INTO telegram_bot_pins VALUES('bot_fixture',12345);").unwrap();
    (
        h,
        Cutoff {
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
    )
}
#[test]
fn journal_apply_repeat_reverse_and_tombstone_are_exact_and_never_activate() {
    let (mut h, c) = fixture();
    let a = apply(&mut h, &c, false).unwrap();
    assert_eq!(a.next_offset, 42);
    assert!(!a.activation_authorized);
    assert_eq!(apply(&mut h, &c, false).unwrap().fingerprint, a.fingerprint);
    assert!(h.view("ag_endpoint").unwrap().paused);
    assert_eq!(apply(&mut h, &c, true).unwrap().next_offset, 0);
    assert_eq!(apply(&mut h, &c, true).unwrap().state, "rolled_back");
    assert!(apply(&mut h, &c, false).is_err());
}
#[test]
fn changed_pins_missing_acceptance_or_activity_refuse_without_offset_change() {
    for scenario in 0..7 {
        let (mut h, mut c) = fixture();
        match scenario {
            0 => c.old_poller_stopped = false,
            1 => c.history_loss_accepted = false,
            2 => c.bot_id += 1,
            3 => c.mappings[0].binding.recipient = "ag_wrong".into(),
            4 => c.old_mailbox_zero_samples = [103, 100],
            5 => {
                h.store
                    .db
                    .execute("UPDATE endpoints SET paused=0", [])
                    .unwrap();
            }
            _ => {
                h.store.db.execute("INSERT INTO hub_activity(endpoint,generation,activated_at) VALUES('ag_endpoint',1,100)",[]).unwrap();
            }
        }
        assert!(apply(&mut h, &c, false).is_err());
        assert_eq!(h.telegram_offset("bot_fixture").unwrap(), 0);
    }
}
#[test]
fn rollback_refuses_new_poll_traffic_and_sql_failure_rolls_back_marker_and_offset() {
    let (mut h, c) = fixture();
    h.store.db.execute_batch("CREATE TRIGGER cutover_failure BEFORE INSERT ON telegram_cutoff_journal BEGIN SELECT RAISE(ABORT,'fixture');END;").unwrap_err();
    // Initialize the additive journal without changing any transport position.
    let mut bad = Cutoff { bot_id: 54321, ..c };
    assert!(apply(&mut h, &bad, false).is_err());
    bad.bot_id = 12345;
    h.store.db.execute_batch("CREATE TRIGGER cutover_failure BEFORE INSERT ON telegram_cutoff_journal BEGIN SELECT RAISE(ABORT,'fixture');END;").unwrap();
    assert!(apply(&mut h, &bad, false).is_err());
    assert_eq!(h.telegram_offset("bot_fixture").unwrap(), 0);
    h.store
        .db
        .execute_batch("DROP TRIGGER cutover_failure")
        .unwrap();
    apply(&mut h, &bad, false).unwrap();
    h.store
        .db
        .execute("UPDATE telegram_accounts SET offset=43,ignored=1", [])
        .unwrap();
    assert!(apply(&mut h, &bad, true).is_err());
    assert_eq!(h.telegram_offset("bot_fixture").unwrap(), 43);
}
