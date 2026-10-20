//! Hermetic process-death rehearsal for transport/storage, not Workers shutdown.
use super::{
    tests::{fake_core, intent_at, setup, URL},
    *,
};
use crate::{
    email::Intent,
    store::{Limits, Store},
};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
const CHILD: &str = "email::delivery::crash_tests::proxy_crash_child";
const FIXTURE_ENV: &str = "TREER_EMAIL_CRASH_FIXTURE";

/// Only this exact spawned child is ever killed/reaped, including failed tests.
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn barrier(root: &Path) -> ! {
    fs::write(root.join("ready"), b"synthetic crash boundary").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "parent did not terminate crash fixture"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn proxy_crash_child() {
    let Some(root) = std::env::var_os(FIXTURE_ENV) else {
        return;
    };
    let root = PathBuf::from(root).canonicalize().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("fixture-kind")).unwrap(),
        "synthetic-email-crash"
    );
    let stage = fs::read_to_string(root.join("stage")).unwrap();
    assert!(matches!(
        stage.as_str(),
        "before_reservation_commit"
            | "before_post"
            | "before_completion_commit"
            | "after_completion_commit"
    ));
    let proxy = fs::read_to_string(root.join("proxy")).unwrap();
    let url = reqwest::Url::parse(&proxy).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let core = fake_core(&root, true); // Always synthetic, never an installed CLI.
    let mut store = Store::open(&root.join("state"), Limits::default()).unwrap();
    setup(&mut store);
    let now = wall_time().unwrap();
    let intent = {
        let mut queue = Queue::new(&mut store).unwrap();
        let intent = intent_at(&mut queue, 1, now);
        queue.prepare_proxy_delivery(&intent, URL, now).unwrap();
        intent
    };
    fs::write(root.join("intent-id"), &intent.id).unwrap();
    if stage == "before_reservation_commit" {
        let root = root.clone();
        // Inside the UPDATE but before COMMIT, retaining the real #217 hook.
        store.db.update_hook(Some(
            move |_: rusqlite::hooks::Action, _: &str, table: &str, _: i64| {
                if table == "email_dispatch" {
                    barrier(&root)
                }
            },
        ));
    }
    let attempt = Queue::new(&mut store)
        .unwrap()
        .begin_proxy_delivery_now(&intent, URL)
        .unwrap();
    if stage == "before_post" {
        barrier(&root)
    }
    let sender = crate::email::proxy::Sender::new(&proxy, "svc_hub").unwrap();
    let completion = sender.send_committed(&core, attempt, || {
        Queue::new(&mut store)
            .unwrap()
            .proxy_dispatch_allowed_now(&intent, URL)
    });
    if stage == "before_completion_commit" {
        let root = root.clone();
        // Only ProviderAccepted updates email_intents. Reaching this hook
        // proves an accepted Completion, while neither write is committed.
        store.db.update_hook(Some(
            move |_: rusqlite::hooks::Action, _: &str, table: &str, _: i64| {
                if table == "email_intents" {
                    barrier(&root)
                }
            },
        ));
    }
    completion
        .commit(&mut Queue::new(&mut store).unwrap())
        .unwrap();
    assert_eq!(
        stage, "after_completion_commit",
        "expected pre-COMMIT barrier was not reached"
    );
    let accepted: bool = store
        .db
        .query_row(
            "SELECT state='provider_accepted' FROM email_dispatch",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(accepted);
    barrier(&root)
}

fn wait_ready(root: &Path, child: &mut OwnedChild) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !root.join("ready").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "crash child exited before its boundary"
        );
        assert!(
            Instant::now() < deadline,
            "crash child did not reach boundary"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert!(child.0.try_wait().unwrap().is_none());
}
fn read_request(listener: &TcpListener, child: &mut OwnedChild) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "crash child exited before POST"
                );
                assert!(Instant::now() < deadline, "crash child never POSTed");
                thread::sleep(Duration::from_millis(2));
            }
            Err(e) => panic!("fake listener failed: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap(); // macOS inherits listener mode.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let payload = read_http(&mut stream);
    let body = r#"{"status":"sent","email_id":"email_synthetic"}"#;
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    payload
}
fn read_http(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        bytes.extend(byte);
        assert!(bytes.len() < 16384);
    }
    let head = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
    assert!(head.starts_with("post /api/apps/svc_hub/email http/1.1\r\n"));
    assert!(head.contains("authorization: bearer synthetic.app.signature\r\n"));
    let size = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length: "))
        .unwrap()
        .parse::<usize>()
        .unwrap();
    assert!(size <= 4096);
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    body
}
fn no_connection(listener: &TcpListener) {
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
}
fn kill_and_reap(child: &mut OwnedChild) {
    child.0.kill().unwrap(); // Unix Child::kill targets this child with SIGKILL.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert_eq!(status.signal(), Some(9));
            break;
        }
        assert!(Instant::now() < deadline, "killed child was not reaped");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn killed_process_contains_ambiguous_post_and_preserves_committed_acceptance() {
    for stage in [
        "before_reservation_commit",
        "before_post",
        "before_completion_commit",
        "after_completion_commit",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("fixture-kind"), "synthetic-email-crash").unwrap();
        fs::write(root.join("stage"), stage).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::write(
            root.join("proxy"),
            format!("http://{}/", listener.local_addr().unwrap()),
        )
        .unwrap();
        let log = fs::File::create(root.join("child.log")).unwrap();
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([CHILD, "--exact", "--test-threads=1"])
                .env(FIXTURE_ENV, &root)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let posted = matches!(
            stage,
            "before_completion_commit" | "after_completion_commit"
        );
        let request = if posted {
            Some(read_request(&listener, &mut child))
        } else {
            None
        };
        let requests = usize::from(request.is_some());
        wait_ready(&root, &mut child);
        let expected_id = fs::read_to_string(root.join("intent-id")).unwrap();
        let path = root.join("state");
        assert!(
            matches!(
                Store::open(&path, Limits::default()),
                Err(crate::store::Error::Busy)
            ),
            "live child must retain private-store ownership"
        );
        // Independent read while the child is alive distinguishes committed
        // state from uncommitted UPDATE-hook handshakes.
        let reader = Connection::open_with_flags(
            path.join("hub.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        reader.busy_timeout(Duration::from_secs(1)).unwrap();
        let (state_before,done_before,frozen):(String,i64,Vec<u8>)=reader.query_row(
            "SELECT d.state,i.state,d.payload FROM email_dispatch d JOIN email_intents i ON i.id=d.id WHERE i.id=?1",[&expected_id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        let committed = stage == "after_completion_commit";
        let prepared = stage == "before_reservation_commit";
        assert_eq!(
            state_before,
            if prepared {
                "prepared"
            } else if committed {
                "provider_accepted"
            } else {
                "dispatching"
            }
        );
        assert_eq!(done_before, i64::from(committed));
        assert_eq!(
            reserved(&reader, &expected_id).unwrap(),
            !prepared && !committed
        );
        if let Some(actual) = &request {
            assert_eq!(actual, &frozen)
        }
        no_connection(&listener);
        drop(reader);
        kill_and_reap(&mut child);
        let reopened = Store::open(&path, Limits::default()).unwrap();
        let mut store =
            crate::email_workers::tests::recover_private_store_with_startup_lease(reopened);
        let intent=store.db.query_row("SELECT i.id,coalesce(i.nonce,'retired'),i.thread,i.endpoint,i.generation,t.address FROM email_intents i JOIN email_targets t ON t.endpoint=i.endpoint",[],|r|Ok(Intent{
            id:r.get(0)?,nonce:r.get(1)?,thread:r.get(2)?,endpoint:r.get(3)?,generation:r.get(4)?,address:r.get(5)?})).unwrap();
        assert_eq!(intent.id, expected_id);
        let mut queue = Queue::new(&mut store).unwrap();
        let (state_after,done,mode,retained):(String,i64,Option<String>,Vec<u8>)=queue.store.db.query_row(
            "SELECT d.state,i.state,i.mode,d.payload FROM email_dispatch d JOIN email_intents i ON i.id=d.id WHERE i.id=?1",[&expected_id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            state_after,
            if prepared {
                "prepared"
            } else if committed {
                "provider_accepted"
            } else {
                "uncertain"
            }
        );
        assert_eq!(done, i64::from(committed));
        assert_eq!(
            mode.as_deref(),
            if committed { Some("proxy") } else { None }
        );
        assert_eq!(retained, frozen);
        assert_eq!(
            reserved(&queue.store.db, &intent.id).unwrap(),
            !prepared && !committed
        );
        queue
            .store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        let future = wall_time().unwrap() + 61;
        if prepared {
            // A reservation that never committed is safe to reclaim locally.
            // No transport call consumes this new attempt in this rehearsal.
            let reclaimed = queue
                .claim_proxy("ag_email", 1, future, &"c".repeat(64))
                .unwrap()
                .unwrap();
            assert_eq!(reclaimed.id, intent.id);
            queue
                .prepare_proxy_delivery(&reclaimed, URL, future)
                .unwrap();
            let attempt = queue.begin_proxy_delivery(&reclaimed, URL, future).unwrap();
            assert_eq!(attempt.payload(), frozen);
            drop(attempt);
        } else {
            assert!(queue.begin_proxy_delivery_now(&intent, URL).is_err());
            assert!(queue
                .claim_proxy("ag_email", 1, future, &"c".repeat(64))
                .unwrap()
                .is_none());
            assert_eq!(queue.recover_interrupted_proxy_deliveries().unwrap(), 0);
        }
        assert_eq!(requests, usize::from(posted));
        no_connection(&listener); // Retained through recovery and future claim.
    }
}
