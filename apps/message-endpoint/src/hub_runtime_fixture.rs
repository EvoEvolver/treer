//! Same runtime assembly as the native executable, with test-only refresh scheduling.
use super::*;
use crate::{
    protocol::{Envelope, Sender, SenderKind},
    telegram_http::BotClient,
    telegram_workers::Account,
};
use reqwest::blocking::Client;
use serde_json::Value;
use std::{
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
struct Fixture {
    child: Child,
    data: Value,
    client: Client,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().canonicalize().unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = dir.join("fixture.py");
        std::fs::write(
            &source,
            include_str!("../tests/positive_runtime_fixture.py"),
        )
        .unwrap();
        let mut child = Command::new("/usr/bin/python3")
            .arg("-I")
            .arg(&source)
            .arg(&dir)
            .env_clear()
            .env("HOME", &dir)
            .env("TMPDIR", &dir)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Self {
            child,
            data: serde_json::from_str(&line).unwrap(),
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
            _root: root,
        }
    }
    fn status(&self) -> Value {
        self.client
            .get(format!(
                "{}/fixture/status",
                self.data["proxy_url"].as_str().unwrap()
            ))
            .send()
            .unwrap()
            .json()
            .unwrap()
    }
    fn control(&self, path: &str, value: Value) {
        assert!(self
            .client
            .post(format!(
                "{}/fixture/{path}",
                self.data["proxy_url"].as_str().unwrap()
            ))
            .json(&value)
            .send()
            .unwrap()
            .status()
            .is_success());
    }
    fn config(&self, enabled: bool) -> Config {
        let cli = PathBuf::from(self.data["cli"].as_str().unwrap());
        serde_json::from_value(json!({"hub_id":"app_hub","workspace_id":"ws_test","service_id":"svc_hub","proxy_url":self.data["proxy_url"],"public_url":self.data["public_url"],"listen":self.data["listen"],"access":"workspace","activation_enabled":enabled,"cli":cli,"cli_sha256":crate::relay::hash_executable(&cli).unwrap(),"state_directory":self._root.path().canonicalize().unwrap().join("state")})).unwrap()
    }
    fn start(&self, enabled: bool) -> Running {
        self.start_variant(enabled, false)
    }
    fn start_variant(&self, enabled: bool, email: bool) -> Running {
        self.start_email_mode(enabled, email.then_some(crate::email::Mode::Outbox))
    }
    fn start_email_mode(&self, enabled: bool, mode: Option<crate::email::Mode>) -> Running {
        let email = mode.is_some();
        let mut config = self.config(enabled);
        if let Some(mode) = mode {
            config.email = Some(crate::email_workers::Config {
                outbox_directory: self._root.path().canonicalize().unwrap().join("outbox"),
                mode,
            });
        }
        config.validate("app_hub", Some("ws_test")).unwrap();
        let bot = BotClient::with_test_origin_159(
            "bot_fixture".into(),
            format!("12345:{}", "a".repeat(35)),
            12345,
            self.data["bot_origin"].as_str().unwrap().into(),
        );
        let (stop, receive) = tokio::sync::oneshot::channel();
        let readback_gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_gate = readback_gate.clone();
        let join = thread::spawn(move || {
            run_assembled(
                config,
                if email {
                    vec![]
                } else {
                    vec![Account::fixture_account_159(bot)]
                },
                async {
                    let _ = receive.await;
                },
                move |state| CoreWorkers::with_test_readback_gate_159(state, worker_gate),
            )
        });
        let running = Running {
            stop: Some(stop),
            join: Some(join),
            readback_gate,
        };
        wait_until(|| {
            self.client
                .get(format!(
                    "{}health",
                    self.data["public_url"].as_str().unwrap()
                ))
                .send()
                .is_ok_and(|r| r.status().is_success())
        });
        running
    }
    fn settings(&self) -> Value {
        self.client
            .get(format!(
                "{}api/settings",
                self.data["public_url"].as_str().unwrap()
            ))
            .header("X-Treer-Identity-Token", "synthetic.owner.token159")
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap()
    }
    fn command(&self, value: Value) -> Value {
        let action = value["action"].as_str().unwrap_or("invalid").to_owned();
        let url = self.data["public_url"].as_str().unwrap();
        let response = self
            .client
            .post(format!("{url}api/configuration"))
            .header("X-Treer-Identity-Token", "synthetic.owner.token159")
            .header("Origin", url.trim_end_matches('/'))
            .header("X-Endpoint-Request", "1")
            .json(&value)
            .send()
            .unwrap();
        let status = response.status();
        let value: Value = response.json().unwrap();
        assert!(status.is_success(), "{action}: {status}: {value}");
        value
    }
}
struct Running {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<thread::JoinHandle<Result<(), Error>>>,
    readback_gate: Arc<std::sync::atomic::AtomicBool>,
}
impl Running {
    fn release_readbacks(&self) {
        self.readback_gate
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(join) = self.join.take() {
            join.join().unwrap().unwrap();
        }
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
#[track_caller]
fn wait_until(mut check: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < until, "runtime fixture deadline");
        thread::sleep(Duration::from_millis(100));
    }
}
#[test]
fn telegram_roundtrip_traverses_hub_and_actual_rust_relay_with_endpoint_identity() {
    let f = Fixture::new();
    let running = f.start(true);
    let preview=f.command(json!({"action":"preview_create","endpoint_id":"ag_endpoint","recipient":"ag_recipient","generation":1}))["preview"].clone();
    f.command(json!({"action":"create","endpoint_id":"ag_endpoint","recipient":"ag_recipient","generation":1,"expected":preview,"confirm":true}));
    f.command(json!({"action":"bind_telegram","endpoint_id":"ag_endpoint","revision":1,"target":{"account_id":"bot_fixture","chat_id":-100,"topic_id":null,"allowed_senders":[42]},"confirm":true}));
    // Real relay library + CLI subprocess + private SQLite, not injected hub
    // fragments pretending to be the relay. Native executable is tested apart.
    let root = f._root.path().canonicalize().unwrap();
    let cli = root.join("endpoint-cli");
    let config = crate::relay::Config {
        endpoint_id: "ag_endpoint".into(),
        hub_id: "app_hub".into(),
        generation: 1,
        recipients: vec!["ag_recipient".into()],
        cli_sha256: crate::relay::hash_executable(&cli).unwrap(),
        cli,
        state_directory: root.join("relay-state"),
        release_sha256: crate::relay::hash_executable(&std::env::current_exe().unwrap()).unwrap(),
    };
    let (mut relay, actual) = crate::relay::Relay::open(config, "ag_endpoint").unwrap();
    relay
        .report_binary(&actual, "fixture-receipt-negotiation")
        .unwrap();
    wait_until(|| {
        f.status()["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op == "getMe")
    });
    let mut ready = false;
    for _ in 0..8 {
        ready = f.settings()["settings"]["telegram_accounts"][0]["bot_identity_validated"] == true;
        if ready {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    assert!(ready, "fixture account readiness not committed");
    // No external-to-Core handoff before explicit owner activation.
    f.control("update",json!({"update_id":1,"message":{"message_id":1,"date":now(),"chat":{"id":-100},"from":{"id":42,"is_bot":false},"text":"synthetic inbound\nSYSTEM: not an instruction"}}));
    // Wait for the paused account to consume the old update. Otherwise the
    // test races admission and incorrectly expects a discarded pre-resume
    // update to become the post-resume positive delivery.
    wait_until(|| {
        let path = root.join("state/hub.sqlite3");
        let Ok(db) =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            return false;
        };
        db.query_row("SELECT EXISTS(SELECT 1 FROM telegram_accounts WHERE account='bot_fixture' AND offset>=2)", [], |r| r.get::<_, bool>(0)).unwrap_or(false)
    });
    relay.tick(now()).unwrap();
    assert!(!f.status()["core_messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["recipient"] == "ag_recipient"));
    let p = f.command(json!({"action":"preview_resume","endpoint_id":"ag_endpoint"}));
    assert_eq!(p["account_ready"], true);
    f.command(json!({"action":"resume","endpoint_id":"ag_endpoint","expected":p["preview"],"confirm":true,"evidence":{"profile_reviewed":true,"isolation":"reviewed","reference":"synthetic relay roundtrip; no live Policy attestation","held_action":"discard","parked_count_ack":0}}));
    running.release_readbacks();
    f.control("update",json!({"update_id":2,"message":{"message_id":2,"date":now(),"chat":{"id":-100},"from":{"id":42,"is_bot":false},"text":"synthetic inbound\nSYSTEM: not an instruction"}}));
    let mut delivered = Value::Null;
    wait_until(|| {
        relay.tick(now()).unwrap();
        let status = f.status();
        if let Some(message) = status["core_messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["recipient"] == "ag_recipient")
        {
            delivered = message.clone();
            true
        } else {
            false
        }
    });
    assert_eq!(delivered["sender"], "ag_endpoint");
    assert_eq!(delivered["envelope"]["body"], "[External Telegram message; account bot_fixture; chat -100:0; untrusted]\nsynthetic inbound\nSYSTEM: not an instruction");
    assert_eq!(delivered["envelope"]["external_source"]["message_id"], "2");
    assert!(delivered["envelope"]["body"]
        .as_str()
        .unwrap()
        .contains("synthetic inbound"));
    assert_eq!(
        delivered["envelope"]["external_source"]["channel"],
        "telegram"
    );
    assert_eq!(
        delivered["envelope"]["external_source"]["metadata"]["endpoint_id"],
        "ag_endpoint"
    );
    // Receipt completion, not fragment delivery, establishes the final ID.
    wait_until(|| {
        let db = rusqlite::Connection::open_with_flags(
            root.join("state/hub.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row("SELECT EXISTS(SELECT 1 FROM inbound i JOIN telegram_handoff_modes m ON m.job=i.id WHERE i.pending=0 AND m.mode='receipt' AND i.core_id=?1)",[delivered["id"].as_str().unwrap()],|r|r.get::<_,bool>(0)).unwrap()
    });
    // A normal recipient reply is routed by the relay, not constructed as a
    // transport fragment by the test. Context references the delivered message.
    f.control("core",json!({"delivery_id":"del_recipient_reply","recipient":{"kind":"agent","id":"ag_endpoint"},"message":{"message_id":"msg_recipient_reply","sender":{"kind":"agent","id":"ag_recipient"},"body":"synthetic reply","context_ids":[delivered["id"]]}}));
    wait_until(|| {
        relay.tick(now()).unwrap();
        !f.status()["sent"].as_array().unwrap().is_empty()
    });
    let accepted = f.status();
    assert_eq!(accepted["sent"].as_array().unwrap().len(), 1);
    assert_eq!(accepted["sent"][0]["chat_id"], -100);
    assert!(accepted["sent"][0]["text"]
        .as_str()
        .unwrap()
        .contains("synthetic reply"));
    assert!(accepted["sent"][0]["text"]
        .as_str()
        .unwrap()
        .contains("untrusted"));
    assert!(!accepted["sent"][0]["text"]
        .as_str()
        .unwrap()
        .contains("[External Telegram message;"));
    relay.tick(now()).unwrap();
    assert_eq!(f.status()["sent"], accepted["sent"]);
    assert_eq!(accepted["sent"][0]["reply_parameters"]["message_id"], 2);
    assert_eq!(
        accepted["sent"][0]["reply_parameters"]["allow_sending_without_reply"],
        false
    );
    // The actual accepted native output is mapped back to its fixed Core message.
    wait_until(|| {
        let db = rusqlite::Connection::open_with_flags(
            root.join("state/hub.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row("SELECT EXISTS(SELECT 1 FROM telegram_reply_mappings WHERE native_id=101 AND core_id='msg_recipient_reply')",[],|r|r.get::<_,bool>(0)).unwrap()
    });
    f.control("update",json!({"update_id":3,"message":{"message_id":3,"reply_to_message":{"message_id":101},"date":now(),"chat":{"id":-100},"from":{"id":42,"is_bot":false},"text":"known native reply"}}));
    wait_until(|| {
        relay.tick(now()).unwrap();
        f.status()["core_messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| {
                m["recipient"] == "ag_recipient"
                    && m["context_ids"] == json!(["msg_recipient_reply"])
            })
    });
    let before = f.status()["core_messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["recipient"] == "ag_recipient")
        .count();
    f.control("update",json!({"update_id":4,"message":{"message_id":4,"reply_to_message":{"message_id":999999},"date":now(),"chat":{"id":-100},"from":{"id":42,"is_bot":false},"text":"OLD SECRET BODY MUST NOT BECOME TOP LEVEL"}}));
    wait_until(|| {
        f.status()["sent"].as_array().unwrap().iter().any(|m| {
            m["text"]
                .as_str()
                .is_some_and(|s| s.contains("history_unavailable"))
        })
    });
    let after = f.status();
    assert_eq!(
        after["core_messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["recipient"] == "ag_recipient")
            .count(),
        before
    );
    assert!(!after["sent"].to_string().contains("OLD SECRET BODY"));
    // Route an exact changed final-ID assertion through the real authenticated
    // inbox worker, local disposition commit, Core ACK and confirmed marker.
    let wire = after["core_messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["recipient"] == "app_hub"
                && m["envelope"]["body"].as_str().is_some_and(|b| {
                    serde_json::from_str::<Value>(b)
                        .ok()
                        .is_some_and(|v| v["protocol"] == crate::telegram_receipts::CAPABILITY)
                })
        })
        .unwrap()["envelope"]["body"]
        .as_str()
        .unwrap();
    let mut receipt: Value = serde_json::from_str(wire).unwrap();
    receipt["final_core_message_id"] = json!("msg_changed_final");
    f.control("core",json!({"delivery_id":"del_terminal_changed","recipient":{"kind":"agent","id":"app_hub"},"message":{"message_id":"msg_terminal_changed","sender":{"kind":"agent","id":"ag_endpoint"},"body":receipt.to_string(),"context_ids":[]}}));
    wait_until(|| {
        let db = rusqlite::Connection::open_with_flags(
            root.join("state/hub.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row("SELECT EXISTS(SELECT 1 FROM telegram_terminal_dispositions WHERE acked=1 AND conflict=0)",[],|r|r.get::<_,bool>(0)).unwrap()
    });
    drop(running);
    let resumed = f.start(true);
    resumed.release_readbacks();
    // Terminal queue and confirmed discard survive the real runtime restart.
    let db = rusqlite::Connection::open_with_flags(
        root.join("state/hub.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM telegram_terminal_dispositions WHERE acked=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM inbound WHERE pending=0 AND core_id=?1)",
            [delivered["id"].as_str().unwrap()],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    drop(db);
    drop(resumed);
}
#[test]
fn owner_activation_real_runtime_http_bot_core_sqlite_and_restart_keep_binding_active() {
    let f = Fixture::new();
    let running = f.start(true);
    let preview=f.command(json!({"action":"preview_create","endpoint_id":"ag_endpoint","recipient":"ag_recipient","generation":1}))["preview"].clone();
    f.command(json!({"action":"create","endpoint_id":"ag_endpoint","recipient":"ag_recipient","generation":1,"expected":preview,"confirm":true}));
    f.command(json!({"action":"bind_telegram","endpoint_id":"ag_endpoint","revision":1,"target":{"account_id":"bot_fixture","chat_id":-100,"topic_id":null,"allowed_senders":[42]},"confirm":true}));
    wait_until(|| {
        f.status()["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "getMe")
    });
    // Seeing getMe at the fake server precedes the worker committing readiness.
    // Poll bounded owner status; do not weaken the production cooldown gate.
    let mut ready = false;
    for _ in 0..8 {
        let snapshot = f.settings();
        assert_eq!(snapshot["settings"]["workers"]["readback_completions"], 0);
        ready = snapshot["settings"]["telegram_accounts"][0]["bot_identity_validated"] == true;
        if ready {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    assert!(ready, "fixture account did not become ready");
    let p = f.command(json!({"action":"preview_resume","endpoint_id":"ag_endpoint"}));
    assert_eq!(p["account_ready"], true);
    f.command(json!({"action":"resume","endpoint_id":"ag_endpoint","expected":p["preview"],"confirm":true,"evidence":{"profile_reviewed":true,"isolation":"reviewed","reference":"synthetic fixture only; not deployment evidence","held_action":"discard","parked_count_ack":0}}));
    // Owner's own real Core queries remain required. Only the unrelated worker
    // refresh waits; the separate controlled-interleave test verifies its racing
    // newer observation correctly rejects an obsolete owner request.
    running.release_readbacks();
    f.control("update",json!({"update_id":1,"message":{"message_id":1,"date":now(),"chat":{"id":-100},"from":{"id":42,"is_bot":false},"text":"synthetic inbound"}}));
    let env = Envelope::Outbound {
        delivery_id: "del_origin".into(),
        message_id: "msg_origin".into(),
        original_sender: Sender {
            kind: SenderKind::Agent,
            id: "ag_recipient".into(),
        },
        body: "synthetic outbound".into(),
        context_ids: vec![],
    };
    for (n, wire) in env
        .fragments("ag_endpoint", 1)
        .unwrap()
        .into_iter()
        .enumerate()
    {
        f.control("core",json!({"delivery_id":format!("del_part_{n}"),"recipient":{"kind":"agent","id":"app_hub"},"message":{"message_id":format!("msg_part_{n}"),"sender":{"kind":"agent","id":"ag_endpoint"},"body":wire,"context_ids":[]}}));
    }
    wait_until(|| {
        let s = f.status();
        !s["core_sent"].as_array().unwrap().is_empty()
            && !s["sent"].as_array().unwrap().is_empty()
            && s["acked"].as_u64().unwrap() > 0
    });
    // A fake upstream observing the request is not a local commit. Wait for
    // durable accepted counters, offset and Core handoff completion separately.
    wait_until(|| {
        let path = f
            ._root
            .path()
            .canonicalize()
            .unwrap()
            .join("state/hub.sqlite3");
        let Ok(db) =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            return false;
        };
        db.query_row("SELECT EXISTS(SELECT 1 FROM hub_activity WHERE endpoint='ag_endpoint' AND inbound_updates=1 AND outbound_chunks=1) AND NOT EXISTS(SELECT 1 FROM inbound WHERE pending=1) AND EXISTS(SELECT 1 FROM telegram_accounts WHERE account='bot_fixture' AND offset=2)",[],|r|r.get::<_,bool>(0)).unwrap_or(false)
    });
    let before = f.settings();
    assert!(
        before["settings"]["workers"]["readback_completions"]
            .as_u64()
            .unwrap()
            > 0
    );
    let endpoint = &before["settings"]["endpoints"][0];
    assert_eq!(endpoint["paused"], false);
    assert_eq!(endpoint["activity"]["inbound_updates"], 1);
    assert_eq!(endpoint["activity"]["outbound_chunks"], 1);
    assert_eq!(before["settings"]["workers"]["cli_integrity_failed"], false);
    let accepted = f.status();
    assert!(accepted["sent"][0]["text"]
        .as_str()
        .unwrap()
        .contains("synthetic outbound"));
    // The real Core CLI fixture captured an inbound protocol fragment, not a direct
    // send as the recipient Agent. This is hub handoff, not final Agent delivery.
    let inbound = &accepted["core_sent"][0]["body"];
    let fragment = crate::protocol::Fragment::parse(inbound.as_str().unwrap()).unwrap();
    assert!(fragment.direction == crate::protocol::Direction::Inbound);
    drop(running);
    let restarted = f.start(true);
    restarted.release_readbacks();
    let after = f.settings();
    assert_eq!(after["settings"]["endpoints"][0]["paused"], false);
    assert_eq!(
        after["settings"]["endpoints"][0]["activity"],
        endpoint["activity"]
    );
    thread::sleep(Duration::from_millis(300));
    assert_eq!(f.status()["sent"], accepted["sent"]);
    assert_eq!(f.status()["core_sent"], accepted["core_sent"]);
    drop(restarted);
    let calls_before = f.status()["operations"].as_array().unwrap().len();
    let disabled = f.start(false);
    disabled.release_readbacks();
    let state = f.settings();
    assert_eq!(state["settings"]["endpoints"][0]["paused"], true);
    assert_eq!(
        state["settings"]["endpoints"][0]["reason"],
        "operator_activation_disabled"
    );
    assert_eq!(
        state["settings"]["telegram_accounts"][0]["state"],
        "operator_disabled"
    );
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        f.status()["operations"].as_array().unwrap().len(),
        calls_before
    );
    drop(disabled);
    eprintln!("PASS: cfg(test) real runtime assembly; actual loopback Bot/owner HTTP, pinned Core CLI, SQLite inbound/outbound counters, active restart and disabled restart; no live bot acceptance");
}

#[test]
fn email_outbox_member_reply_traverses_real_runtime_and_relay_without_real_mail() {
    email_runtime_chain(crate::email::Mode::Outbox, false, false);
}
#[test]
fn email_proxy_runtime_commits_acceptance_and_restarts_without_another_post() {
    email_runtime_chain(crate::email::Mode::Proxy, false, false);
}
#[test]
fn email_proxy_runtime_holds_uncertain_after_restart_without_resend() {
    email_runtime_chain(crate::email::Mode::Proxy, true, false);
}
#[test]
fn email_owner_observation_and_retirement_http_worker_restart_chain() {
    email_runtime_chain(crate::email::Mode::Proxy, true, true);
}
#[test]
fn email_outbox_retirement_worker_unlinks_only_retired_files_after_restart() {
    email_runtime_chain(crate::email::Mode::Outbox, false, true);
}
#[test]
fn email_proxy_review_revoked_during_reserved_token_must_not_post() {
    email_runtime_chain_review(crate::email::Mode::Proxy, false, false, true);
}
fn email_runtime_chain(mode: crate::email::Mode, uncertain: bool, recover: bool) {
    email_runtime_chain_review(mode, uncertain, recover, false);
}
fn email_runtime_chain_review(
    mode: crate::email::Mode,
    uncertain: bool,
    recover: bool,
    review_revoke: bool,
) {
    let f = Fixture::new();
    if uncertain {
        f.control("email-uncertain", json!({"uncertain":true}));
    }
    let running = f.start_email_mode(true, Some(mode));
    let preview=f.command(json!({"action":"preview_create","endpoint_id":"ag_endpoint","recipient":"ag_recipient","generation":1}))["preview"].clone();
    f.command(json!({"action":"create","endpoint_id":"ag_endpoint","recipient":"ag_recipient","generation":1,"expected":preview,"confirm":true}));
    f.command(json!({"action":"bind_email","endpoint_id":"ag_endpoint","revision":1,"contact":{"address":crate::email::REAL_RECIPIENT,"member_id":"user_member"},"confirm":true}));
    let root = f._root.path().canonicalize().unwrap();
    let cli = root.join("endpoint-cli");
    let config = crate::relay::Config {
        endpoint_id: "ag_endpoint".into(),
        hub_id: "app_hub".into(),
        generation: 1,
        recipients: vec!["ag_recipient".into()],
        cli_sha256: crate::relay::hash_executable(&cli).unwrap(),
        cli,
        state_directory: root.join("relay-state"),
        release_sha256: crate::relay::hash_executable(&std::env::current_exe().unwrap()).unwrap(),
    };
    let (mut relay, _) = crate::relay::Relay::open(config, "ag_endpoint").unwrap();
    relay.tick(now()).unwrap();
    let p = f.command(json!({"action":"preview_resume","endpoint_id":"ag_endpoint"}));
    assert_eq!(p["account_ready"], true);
    assert_eq!(p["preview"]["target"]["channel"], "email");
    f.command(json!({"action":"resume","endpoint_id":"ag_endpoint","expected":p["preview"],"confirm":true,"evidence":{"profile_reviewed":true,"isolation":"reviewed","reference":"synthetic email test, no real Policy attestation","held_action":"discard","parked_count_ack":0}}));
    running.release_readbacks();
    if review_revoke {
        std::fs::write(root.join("revoke-on-reserved-token"), b"1").unwrap();
    }
    f.control("core",json!({"delivery_id":"del_email_agent","recipient":{"kind":"agent","id":"ag_endpoint"},"message":{"message_id":"msg_email_agent","sender":{"kind":"agent","id":"ag_recipient"},"body":"<script>private original</script>","context_ids":[]}}));
    if review_revoke {
        wait_until(|| {
            relay.tick(now()).unwrap();
            f.status()["member_revoked"] == true
        });
        wait_until(|| {
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}))
                ["delivery"]["uncertain"]
                == 1
        });
        assert!(
            f.status()["emails"].as_array().unwrap().is_empty(),
            "POST after member revoked during dispatch token mint"
        );
        f.control("revoke-member", json!({"revoked":false}));
        drop(running);
        let restarted = f.start_email_mode(true, Some(mode));
        restarted.release_readbacks();
        assert_eq!(
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}))
                ["delivery"]["uncertain"],
            1
        );
        assert!(f.status()["emails"].as_array().unwrap().is_empty());
        eprintln!("PASS: Core review 7465ba32 reproduction now refuses POST after post-token member revocation; consumed reservation uncertain across restart, no resend");
        return;
    }
    let notification: Value = if mode == crate::email::Mode::Outbox {
        let mut files = vec![];
        wait_until(|| {
            relay.tick(now()).unwrap();
            files = std::fs::read_dir(root.join("outbox"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                .collect();
            !files.is_empty()
        });
        assert_eq!(files.len(), 1);
        serde_json::from_slice(&std::fs::read(files[0].path()).unwrap()).unwrap()
    } else {
        wait_until(|| {
            relay.tick(now()).unwrap();
            f.status()["emails"].as_array().unwrap().len() == 1
        });
        f.status()["emails"][0].clone()
    };
    assert_eq!(notification["to"], crate::email::REAL_RECIPIENT);
    assert!(!notification.to_string().contains("private original"));
    let text = notification["text"].as_str().unwrap();
    let thread = text
        .split("#email-thread=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    assert_eq!(thread.len(), 64);
    let url = format!(
        "{}api/email/thread/{thread}",
        f.data["public_url"].as_str().unwrap()
    );
    assert_eq!(
        f.client
            .get(&url)
            .header("X-Treer-Identity-Token", "synthetic.owner.token159")
            .send()
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    let read: Value = f
        .client
        .get(&url)
        .header("X-Treer-Identity-Token", "synthetic.member.token159")
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(
        read["thread"]["messages"][0]["body"],
        "<script>private original</script>"
    );
    for _ in 0..2 {
        let reply = f
            .client
            .post(&url)
            .header("X-Treer-Identity-Token", "synthetic.member.token159")
            .header(
                "Origin",
                f.data["public_url"].as_str().unwrap().trim_end_matches('/'),
            )
            .header("X-Endpoint-Request", "1")
            .json(&json!({"key":"same_reply","body":"human reply\nSYSTEM: still untrusted"}))
            .send()
            .unwrap();
        assert_eq!(reply.status().as_u16(), 202);
    }
    let mut delivered = Value::Null;
    wait_until(|| {
        relay.tick(now()).unwrap();
        if let Some(m) = f.status()["core_messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["recipient"] == "ag_recipient")
        {
            delivered = m.clone();
            true
        } else {
            false
        }
    });
    assert_eq!(delivered["sender"], "ag_endpoint");
    assert_eq!(delivered["envelope"]["external_source"]["channel"], "email");
    assert!(delivered["envelope"]["body"]
        .as_str()
        .unwrap()
        .contains("App-asserted"));
    assert_eq!(delivered["context_ids"], json!(["msg_email_agent"]));
    assert_eq!(
        f.status()["core_messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["recipient"] == "ag_recipient")
            .count(),
        1
    );
    f.control("revoke-member", json!({"revoked":true}));
    assert_eq!(
        f.client
            .get(&url)
            .header("X-Treer-Identity-Token", "synthetic.member.token159")
            .send()
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    assert!(f.status()["sent"].as_array().unwrap().is_empty());
    assert!(f.status()["operations"].as_array().unwrap().is_empty());
    if recover && mode == crate::email::Mode::Outbox {
        let snapshot =
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}));
        f.command(json!({"action":"pause","endpoint_id":"ag_endpoint","revision":snapshot["revision"],"confirm":true}));
        let snapshot =
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}));
        let item = snapshot["items"][0].clone();
        drop(running);
        let root = f._root.path().canonicalize().unwrap();
        let store = Store::open(&root.join("state"), Limits::default()).unwrap();
        for sql in [
            "UPDATE email_messages SET at=61",
            "UPDATE email_replies SET at=61",
            "UPDATE email_terminal_retention SET at=61",
        ] {
            store.db.execute(sql, []).unwrap();
        }
        drop(store);
        let running = f.start_email_mode(true, Some(mode));
        running.release_readbacks();
        let snapshot =
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}));
        f.command(json!({"action":"purge_email_payload","endpoint_id":"ag_endpoint","revision":snapshot["revision"],"intent_id":item["intent_id"],"confirm":true}));
        wait_until(|| {
            std::fs::read_dir(root.join("outbox"))
                .unwrap()
                .next()
                .is_none()
        });
        drop(running);
        let restarted = f.start_email_mode(true, Some(mode));
        restarted.release_readbacks();
        assert!(std::fs::read_dir(root.join("outbox"))
            .unwrap()
            .next()
            .is_none());
        assert!(
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}))
                ["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(f.status()["emails"].as_array().unwrap().is_empty());
        eprintln!("PASS: real owner HTTP terminal retirement + supervised worker outbox unlink + restart, synthetic Core/member, no real mail, no republish");
        return;
    }
    if mode == crate::email::Mode::Proxy {
        let expected = if uncertain {
            "uncertain"
        } else {
            "provider_accepted"
        };
        wait_until(|| {
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}))
                ["delivery"][expected]
                == 1
        });
        let paused = f.command(json!({"action":"pause","endpoint_id":"ag_endpoint","revision":f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}))["revision"],"confirm":true}));
        assert!(!paused.is_null());
        let inspection =
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}));
        assert_eq!(inspection["paused"], true);
        if uncertain {
            assert_eq!(inspection["items"][0]["abandonable"], false);
        }
        drop(running);
        let restarted = f.start_email_mode(true, Some(mode));
        restarted.release_readbacks();
        let inspection =
            f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}));
        assert_eq!(inspection["delivery"][expected], 1);
        if recover {
            let item = inspection["items"][0].clone();
            let result=f.command(json!({"action":"resolve_email_observed","endpoint_id":"ag_endpoint","revision":inspection["revision"],"intent_id":item["intent_id"],"reference":item["reference"],"confirm":true}));
            assert_eq!(result["delivery"]["uncertain"], 0);
            assert_eq!(result["delivery"]["provider_accepted"], 0);
            assert_eq!(result["paused"], true);
            drop(restarted);
            let root = f._root.path().canonicalize().unwrap();
            // Only this stopped, new synthetic fixture's timestamps are aged.
            let store = Store::open(&root.join("state"), Limits::default()).unwrap();
            store
                .db
                .execute("UPDATE email_messages SET at=61", [])
                .unwrap();
            store
                .db
                .execute("UPDATE email_replies SET at=61", [])
                .unwrap();
            store
                .db
                .execute("UPDATE email_terminal_retention SET at=61", [])
                .unwrap();
            drop(store);
            let final_run = f.start_email_mode(true, Some(mode));
            final_run.release_readbacks();
            let inspection =
                f.command(json!({"action":"inspect_email_delivery","endpoint_id":"ag_endpoint"}));
            let purged=f.command(json!({"action":"purge_email_payload","endpoint_id":"ag_endpoint","revision":inspection["revision"],"intent_id":item["intent_id"],"confirm":true}));
            assert_eq!(purged["items"].as_array().unwrap().len(), 0);
            assert_eq!(purged["paused"], true);
            assert_eq!(f.status()["emails"].as_array().unwrap().len(), 1);
            drop(final_run);
            let store = Store::open(&root.join("state"), Limits::default()).unwrap();
            let retained:(i64,i64,i64)=store.db.query_row("SELECT (SELECT count(*) FROM email_owner_resolutions),(SELECT count(*) FROM email_purged_threads),(SELECT sum(length(body)) FROM email_messages)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            assert_eq!(retained, (1, 1, 0));
            eprintln!("PASS: synthetic Core/mail transport, real owner HTTP + Rust worker + SQLite: uncertain POST once, restart, owner observation (not provider acceptance), restart, terminal payload purge, retained tombstones; zero resend");
        }
        assert_eq!(f.status()["emails"].as_array().unwrap().len(), 1);
        assert_eq!(
            f.status()["core_messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["recipient"] == "ag_recipient")
                .count(),
            1
        );
    }
}

#[test]
fn telegram_stopped_cutoff_handoff_and_pretraffic_rollback_never_overlap_pollers() {
    use crate::telegram_cutoff::{self, Cutoff, Mapping};
    let f = Fixture::new();
    let running = f.start(true);
    let preview=f.command(json!({"action":"preview_create","endpoint_id":"ag_endpoint","recipient":"ag_recipient","generation":1}))["preview"].clone();
    f.command(json!({"action":"create","endpoint_id":"ag_endpoint","recipient":"ag_recipient","generation":1,"expected":preview,"confirm":true}));
    let target = crate::telegram::Target {
        account_id: "bot_fixture".into(),
        chat_id: -100,
        topic_id: None,
        allowed_senders: vec![42],
    };
    f.command(json!({"action":"bind_telegram","endpoint_id":"ag_endpoint","revision":1,"target":target,"confirm":true}));
    let view = f.settings()["settings"]["endpoints"][0].clone();
    let state = f._root.path().canonicalize().unwrap().join("state");
    assert!(
        Store::open(&state, Limits::default()).is_err(),
        "live process lock must refuse offline import"
    );
    drop(running);
    wait_until(|| f.status()["active_pollers"] == 0);
    struct Poller {
        stop: Arc<std::sync::atomic::AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }
    impl Drop for Poller {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }
    let spawn_old = |actor: &'static str| {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = stop.clone();
        let client = f.client.clone();
        let url = format!(
            "{}/bot12345:{}/getUpdates",
            f.data["bot_origin"].as_str().unwrap(),
            "a".repeat(35)
        );
        let join = thread::spawn(move || {
            while !signal.load(std::sync::atomic::Ordering::Acquire) {
                client
                    .post(&url)
                    .header("X-Fixture-Poller", actor)
                    .json(&json!({"offset":42}))
                    .send()
                    .unwrap()
                    .error_for_status()
                    .unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        });
        Poller {
            stop,
            join: Some(join),
        }
    };
    let old = spawn_old("legacy");
    wait_until(|| {
        f.status()["poll_trace"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["actor"] == "legacy")
    });
    drop(old);
    assert_eq!(f.status()["active_pollers"], 0);
    let cutoff = Cutoff {
        inventory_sha256: "a".repeat(64),
        bot_id: 12345,
        account_id: "bot_fixture".into(),
        expected_offset: 0,
        legacy_last_update_id: 41,
        mappings: vec![Mapping {
            binding: crate::hub::Binding {
                endpoint_id: "ag_endpoint".into(),
                generation: 1,
                recipient: "ag_recipient".into(),
                expected_path: "/team/ag_endpoint".into(),
                expected_cwd: "/private/fixture".into(),
                expected_server_id: "machine_fixture".into(),
            },
            revision: view["revision"].as_i64().unwrap(),
            target,
        }],
        history_loss_accepted: true,
        app_asserted_receipts_accepted: true,
        old_poller_stopped: true,
        senders_fenced: true,
        old_mailbox_zero_samples: [100, 103],
        legacy_outbox_drained: true,
    };
    let request_path = f._root.path().canonicalize().unwrap().join("cutoff.json");
    let write_request = |rollback| {
        std::fs::write(&request_path,serde_json::to_vec(&json!({"state_directory":state,"hub_id":"app_hub","workspace_id":"ws_test","rollback":rollback,"cutoff":cutoff})).unwrap()).unwrap();
        std::fs::set_permissions(&request_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    write_request(false);
    assert_eq!(
        telegram_cutoff::run_file(&request_path)
            .unwrap()
            .next_offset,
        42
    );
    let new = f.start(true);
    wait_until(|| {
        f.status()["poll_trace"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["actor"] == "hub" && p["offset"] == 42)
    });
    assert!(telegram_cutoff::run_file(&request_path).is_err());
    assert_eq!(f.settings()["settings"]["endpoints"][0]["paused"], true);
    drop(new);
    wait_until(|| f.status()["active_pollers"] == 0);
    write_request(true);
    assert_eq!(
        telegram_cutoff::run_file(&request_path)
            .unwrap()
            .next_offset,
        0
    );
    let old = spawn_old("legacy_rollback");
    wait_until(|| {
        f.status()["poll_trace"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["actor"] == "legacy_rollback" && p["offset"] == 42)
    });
    drop(old);
    let status = f.status();
    assert_eq!(status["active_pollers"], 0);
    assert_eq!(status["max_pollers"], 1);
    assert!(status["sent"].as_array().unwrap().is_empty());
    assert!(status["core_messages"].as_array().unwrap().is_empty());
    eprintln!("PASS local HTTP poller handoff: synthetic legacy join -> real stopped-store journal -> actual Hub polling offset42 paused -> joined Hub -> pretraffic journal reverse -> synthetic legacy offset42; max concurrent polls1; no real bot/Core/platform acceptance");
}
