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
        let config = self.config(enabled);
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
                vec![Account::fixture_account_159(bot)],
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
    let (mut relay, _) = crate::relay::Relay::open(config, "ag_endpoint").unwrap();
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
    relay.tick(now()).unwrap();
    assert!(f.status()["core_messages"].as_array().unwrap().is_empty());
    let p = f.command(json!({"action":"preview_resume","endpoint_id":"ag_endpoint"}));
    assert_eq!(p["account_ready"], true);
    f.command(json!({"action":"resume","endpoint_id":"ag_endpoint","expected":p["preview"],"confirm":true,"evidence":{"profile_reviewed":true,"isolation_reviewed":true,"reference":"synthetic relay roundtrip; no live Policy attestation","held_action":"discard","parked_count_ack":0}}));
    running.release_readbacks();
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
    relay.tick(now()).unwrap();
    assert_eq!(f.status()["sent"], accepted["sent"]);
    drop(running);
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
    f.command(json!({"action":"resume","endpoint_id":"ag_endpoint","expected":p["preview"],"confirm":true,"evidence":{"profile_reviewed":true,"isolation_reviewed":true,"reference":"synthetic fixture only; not deployment evidence","held_action":"discard","parked_count_ack":0}}));
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
