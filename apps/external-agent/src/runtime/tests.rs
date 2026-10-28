use super::*;
use super::{http::Response, server::App};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const OWN: &str = "ag_11111111111111111111111111111111";
const TARGET: &str = "ag_22222222222222222222222222222222";
const AGENT: &str = "ag_33333333333333333333333333333333";
const WORKLOAD: &str = "synthetic-workload-credential-only";
/// Stand-in for the native `treer` CLI: process-boundary behavior only.
const CLI: &str = r#"#!/usr/bin/python3
import sys,os,time,json
a=sys.argv[1:]
d=os.path.dirname(os.path.abspath(__file__))
if a==['--version']:
    print('treer 0.2.0 (fixture)'); sys.exit(0)
open(os.path.join(d,'cli.log'),'a').write(json.dumps(a)+'\n')
if a[0]=='block':
    open(os.path.join(d,'entered'),'w').close()
    end=time.time()+5
    while not os.path.exists(os.path.join(d,'release')) and time.time()<end: time.sleep(0.01)
if a[0]=='sleep': time.sleep(5)
if a[0]=='large': sys.stdout.write('x'*3000000);sys.stdout.flush()
if a[0]=='fork':
    p=os.fork()
    if p==0: time.sleep(5);os._exit(0)
    print(p,flush=True)
elif a[0]=='deny':
    sys.stderr.write('policy_denied'); sys.exit(1)
else: print(json.dumps({'argv':a,'stdin':sys.stdin.read(),'agent':os.environ.get('TREER_AGENT_ID'),'mode':os.environ.get('TREER_IDENTITY_MODE'),'keys':sorted(os.environ)}))
sys.stderr.write('fixture-stderr')
sys.exit(7 if a[0]=='nonzero' else 0)
"#;
struct Listener {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Listener {
    fn start(f: impl Fn(TcpStream) + Send + Sync + 'static) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let address = l.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let f = Arc::new(f);
        let handle = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((s, _)) => {
                        // Darwin inherits O_NONBLOCK from this test accept loop; production
                        // listeners are blocking. Match that contract for bounded write_all.
                        s.set_nonblocking(false).unwrap();
                        let f = f.clone();
                        thread::spawn(move || f(s));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(handle),
        }
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
#[derive(Default)]
struct Backend {
    /// Controller calls other than health and the startup own-Agent read.
    calls: AtomicUsize,
    inbox: Mutex<Vec<Value>>,
    verifies: AtomicUsize,
    /// Whether Controller health advertises `message.endpoint-inbox.v1`.
    no_inbox_capability: AtomicBool,
    lost: AtomicBool,
    oversize: AtomicBool,
    slow_invalid_owner: AtomicBool,
}
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    config: Config,
    app: Arc<App>,
    backend: Arc<Backend>,
    public: Listener,
    private: Listener,
    _proxy: Listener,
    _controller: Listener,
}
fn backend_services(backend: &Arc<Backend>) -> (Listener, Listener) {
    let b = backend.clone();
    let controller = Listener::start(move |mut s| {
        let r = http::read(&mut s).unwrap();
        // No human/external credential may reach Controller.
        assert_eq!(r.header("x-treer-agent-id"), Some(OWN));
        assert_eq!(r.header("x-treer-workload-credential"), Some(WORKLOAD));
        assert!(r.header("authorization").is_none());
        assert!(r.header("x-treer-identity-token").is_none());
        let response = if r.path == "/api/health" {
            let caps: Vec<&str> = if b.no_inbox_capability.load(Ordering::Relaxed) {
                vec!["agent.prompt-delivery.v1"]
            } else {
                vec!["agent.prompt-delivery.v1", "message.endpoint-inbox.v1"]
            };
            Response::json(
                200,
                json!({"service":"treer-agent-server","workspace_id":"ws_fixture","server_id":"machine_fixture","cli_capabilities":caps}),
            )
        } else if r.path == format!("/api/agents/{OWN}") {
            Response::json(200, json!({"agent_id":OWN,"server_id":"machine_fixture"}))
        } else {
            b.calls.fetch_add(1, Ordering::Relaxed);
            if b.lost.load(Ordering::Relaxed) {
                return;
            }
            if b.oversize.load(Ordering::Relaxed) {
                Response::json(200, json!({"private":"X".repeat(RESPONSE_LIMIT+100)}))
            } else if r.path == "/api/messages/receive" {
                let v: Value = serde_json::from_slice(&r.body).unwrap();
                assert_eq!(r.method, "POST");
                assert_eq!(v["wait_milliseconds"], 0);
                assert!(v["kind"].is_null());
                let records: Vec<_> = b
                    .inbox
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|d| d["recipient"]["app_endpoint"] == v["app_endpoint"])
                    .cloned()
                    .collect();
                // Final Core contract: one opaque top-level cursor, absent on an empty page.
                let page = if v["after"] == "cursor_end" {
                    vec![]
                } else {
                    records
                };
                let mut body = json!({"deliveries":page,"remaining_unacknowledged":page.len()});
                if !page.is_empty() {
                    body["next_cursor"] = json!("cursor_end");
                }
                Response::json(200, body)
            } else if r.path == "/api/messages/ack" {
                let v: Value = serde_json::from_slice(&r.body).unwrap();
                let mut records = b.inbox.lock().unwrap();
                let ids = v["delivery_ids"].as_array().unwrap();
                if records.iter().any(|d| {
                    ids.contains(&d["delivery_id"])
                        && d["recipient"]["app_endpoint"] != v["app_endpoint"]
                }) {
                    Response::json(
                        404,
                        json!({"error":{"code":"message_delivery_not_found","message":"PRIVATE-DIAGNOSTIC"}}),
                    )
                } else {
                    records.retain(|d| !ids.contains(&d["delivery_id"]));
                    Response::json(
                        200,
                        json!({"acknowledged_delivery_ids":ids,"already_acknowledged_delivery_ids":[]}),
                    )
                }
            } else {
                panic!("unexpected Controller call {} {}", r.method, r.path)
            }
        };
        http::write(&mut s, response, "'none'");
    });
    let b = backend.clone();
    let proxy = Listener::start(move |mut s| {
        let r = http::read(&mut s).unwrap();
        assert_eq!(
            (&*r.method, &*r.path),
            ("POST", "/.treer/apps/identity/verify")
        );
        let value: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(value["audience"], "svc_fixture");
        b.verifies.fetch_add(1, Ordering::Relaxed);
        let token = value["token"].as_str().unwrap();
        if token == "invalid" && b.slow_invalid_owner.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(250));
        }
        let t = now().unwrap();
        let mut claims = json!({"sub":"user_owner","aud":"svc_fixture","workspace_id":"ws_fixture","service_id":"svc_fixture","principal_kind":"human","role":"owner","iat":t,"exp":t+60});
        match token {
            "owner" => {}
            "member" => claims["role"] = json!("member"),
            "agent" => {
                claims["principal_kind"] = json!("agent");
                claims["sub"] = json!(AGENT);
                claims.as_object_mut().unwrap().remove("role");
            }
            "self_agent" => {
                claims["principal_kind"] = json!("agent");
                claims["sub"] = json!(OWN);
                claims.as_object_mut().unwrap().remove("role");
            }
            "expired" => claims["exp"] = json!(t - 1),
            "oauth" => claims["exp"] = json!(t + 43200),
            "wrong_audience" => claims["aud"] = json!("other"),
            "wrong_workspace" => claims["workspace_id"] = json!("ws_other"),
            "invalid" => {}
            _ => panic!("unexpected fixture assertion"),
        }
        http::write(
            &mut s,
            Response::json(200, json!({"active":token!="invalid","claims":claims})),
            "'none'",
        );
    });
    (controller, proxy)
}
fn config_for(
    root: &Path,
    controller: &Listener,
    proxy: &Listener,
    cli: Option<PathBuf>,
) -> Config {
    Config {
        workspace: "ws_fixture".into(),
        app: "app_fixture".into(),
        service: "svc_fixture".into(),
        agent: OWN.into(),
        machine: "machine_fixture".into(),
        controller: format!("http://{}/", controller.address).parse().unwrap(),
        proxy: format!("http://{}/", proxy.address).parse().unwrap(),
        public_origin: "https://data.example/".parse().unwrap(),
        owner_origin: "https://external.example/".parse().unwrap(),
        console_origin: "https://console.example/".parse().unwrap(),
        tunnel_origin: "https://proxy.example/".parse().unwrap(),
        console_url: "https://console.example/workspaces/ws_fixture/"
            .parse()
            .unwrap(),
        listen: "127.0.0.1:0".parse().unwrap(),
        owner_listen: "127.0.0.1:0".parse().unwrap(),
        state: root.join("state"),
        treer_cli: cli,
        legacy_policy_acceptance_sha256: None,
        legacy_prompt_review_sha256: None,
    }
}
impl Fixture {
    fn new() -> Self {
        Self::build(true, false)
    }
    fn build(gateway: bool, no_inbox_capability: bool) -> Self {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let backend = Arc::new(Backend::default());
        backend
            .no_inbox_capability
            .store(no_inbox_capability, Ordering::Relaxed);
        let (controller, proxy) = backend_services(&backend);
        let cli = gateway.then(|| {
            let executable = root.join("treer");
            std::fs::write(&executable, CLI).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            executable
        });
        let config = config_for(&root, &controller, &proxy, cli);
        let app = App::open(config.clone(), WORKLOAD.into()).unwrap();
        let a = app.clone();
        let public = Listener::start(move |s| a.accept(s));
        let a = app.clone();
        let private = Listener::start(move |s| a.accept_owner(s));
        Self {
            _dir: d,
            root,
            config,
            app,
            backend,
            public,
            private,
            _proxy: proxy,
            _controller: controller,
        }
    }
    /// Number of native CLI invocations (excluding `--version` probes).
    fn cli_calls(&self) -> usize {
        std::fs::read_to_string(self.root.join("cli.log"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
    fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (u16, Value) {
        call(
            if path.starts_with("/owner/") {
                self.private.address
            } else {
                self.public.address
            },
            method,
            path,
            headers,
            body,
        )
    }
    fn owner(&self, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call(
            if body.is_some() { "POST" } else { "GET" },
            path,
            &[
                ("x-treer-identity-token", "owner"),
                ("origin", "https://external.example"),
                ("sec-fetch-site", "same-origin"),
                ("x-external-agent-csrf", "1"),
            ],
            body,
        )
    }
    /// A workspace Agent calling the management API through its runtime proxy: no browser
    /// headers; the proxy sends the identity both as the identity header and as Bearer.
    fn agent(&self, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call(
            if body.is_some() { "POST" } else { "GET" },
            path,
            &[
                ("x-treer-identity-token", "agent"),
                ("authorization", "Bearer agent"),
            ],
            body,
        )
    }
    fn create(&self) -> String {
        let (s, v) = self.owner(
            "/owner/tokens",
            Some(json!({"name":"fixture","days":30,"authority_confirmed":true})),
        );
        assert_eq!(s, 201, "{v}");
        v["plaintext"].as_str().unwrap().into()
    }
    fn enable(&self) {
        let (s, v) = self.owner("/owner/state", None);
        assert_eq!(s, 200);
        let(s,v)=self.owner("/owner/activation",Some(json!({"enabled":true,"generation":v["generation"],"agent":OWN,"policy_reviewed":true})));
        assert_eq!(s, 200, "{v}");
    }
    fn register(&self, key: &str) -> String {
        let (s, v) = self.owner(
            "/owner/external-agents",
            Some(json!({"key":key,"name":key,"days":30,"authority_confirmed":true})),
        );
        assert_eq!(s, 201, "{v}");
        v["plaintext"].as_str().unwrap().into()
    }
    fn external(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call(
            method,
            path,
            &[("authorization", &format!("Bearer {token}"))],
            body,
        )
    }
    /// One ordinary gateway call (`treer agent ls`).
    fn cli(&self, token: &str) -> (u16, Value) {
        self.external(
            token,
            "POST",
            "/v1/cli",
            Some(json!({"argv":["agent","ls"]})),
        )
    }
}
fn call(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, Value) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(14))).unwrap();
    let body = body
        .map(|v| serde_json::to_vec(&v).unwrap())
        .unwrap_or_default();
    let mut h=format!("{method} {path} HTTP/1.1\r\nHost: fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",body.len());
    for (k, v) in headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    s.write_all(h.as_bytes()).unwrap();
    s.write_all(&body).unwrap();
    let mut b = Vec::new();
    s.read_to_end(&mut b).unwrap();
    let end = b.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
    let h = std::str::from_utf8(&b[..end]).unwrap();
    assert!(h.contains("Cache-Control: no-store"));
    assert!(!h.to_lowercase().contains("access-control-allow"));
    let status = h.split_whitespace().nth(1).unwrap().parse().unwrap();
    let value = serde_json::from_slice(&b[end + 4..]).unwrap_or_else(|e| {
        if path == "/v1/cli" {
            panic!(
                "CLI json invalid: bytes={} header={} parse={e}",
                b.len() - end - 4,
                h
            );
        }
        json!({"text":String::from_utf8_lossy(&b[end+4..])})
    });
    (status, value)
}
#[test]
fn owner_token_cli_chain_audit_and_core_refusal() {
    let f = Fixture::new();
    let token = f.create();
    assert_eq!(f.cli(&token).0, 503);
    f.enable();
    let (s, v) = f.cli(&token);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["result"]["exit_code"], 0);
    // Core/Policy refusal surfaces as the CLI's own nonzero exit, never retried.
    let (s, v) = f.external(&token, "POST", "/v1/cli", Some(json!({"argv":["deny"]})));
    assert_eq!(s, 200);
    assert_eq!(v["result"]["exit_code"], 1);
    assert_eq!(v["result"]["stderr"], "policy_denied");
    assert_eq!(f.cli_calls(), 2);
    let (_, state) = f.owner("/owner/state", None);
    assert_eq!(state["tokens"][0]["ingress_peer"], "127.0.0.1");
    assert_eq!(state["caller"]["kind"], "human");
    let (status, audit) = f.owner("/owner/audit", None);
    assert_eq!(status, 200);
    assert_eq!(audit["identity"]["agent"], OWN);
    assert_eq!(audit["requests"].as_array().unwrap().len(), 2);
    for view in [&state, &audit] {
        assert!(!view.to_string().contains(&token));
        assert!(!view.to_string().contains(&token[38..]));
    }
}
#[test]
fn agent_identity_operates_the_owner_api_except_device_decision() {
    let f = Fixture::new();
    // A workspace Agent: no Origin/CSRF headers, authorized by Core issuing its identity.
    let (s, state) = f.agent("/owner/state", None);
    assert_eq!(s, 200, "{state}");
    assert_eq!(state["caller"]["kind"], "agent");
    let (s, created) = f.agent(
        "/owner/tokens",
        Some(json!({"name":"by-agent","days":30,"authority_confirmed":true})),
    );
    assert_eq!(s, 201, "{created}");
    let token = created["plaintext"].as_str().unwrap().to_owned();
    let (s, v) = f.agent(
        "/owner/activation",
        Some(json!({"enabled":true,"generation":state["generation"],"agent":OWN,"policy_reviewed":true})),
    );
    assert_eq!(s, 200, "{v}");
    assert_eq!(f.cli(&token).0, 200);
    let (s, v) = f.agent(
        "/owner/external-agents",
        Some(json!({"key":"robot","name":"Robot","days":30,"authority_confirmed":true})),
    );
    assert_eq!(s, 201, "{v}");
    assert_eq!(
        f.agent("/owner/external-agents", None).1["agents"][0]["key"],
        "robot"
    );
    let (s, audit) = f.agent("/owner/audit", None);
    assert_eq!(s, 200);
    assert_eq!(audit["requests"].as_array().unwrap().len(), 1);
    // The actor recorded for agent-created credentials is the Agent's own id.
    assert_eq!(
        f.app
            .store
            .lock()
            .unwrap()
            .db_creator(&token[5..37])
            .unwrap(),
        AGENT
    );
    let (_, request) = f.call(
        "POST",
        "/v1/device/request",
        &[],
        Some(json!({"key":"device","name":"Device"})),
    );
    let code = request["user_code"].as_str().unwrap();
    let (s, v) = f.agent("/owner/device/requests", None);
    assert_eq!(s, 200);
    assert_eq!(v["requests"][0]["key"], "device");
    assert_eq!(v["requests"][0]["status"], "pending");
    assert!(!v.to_string().contains(code));
    assert_eq!(
        f.agent("/owner/device/lookup", Some(json!({"user_code":code})))
            .0,
        200
    );
    let (s, v) = f.agent(
        "/owner/device/decision",
        Some(json!({"user_code":code,"approve":true,"days":30,"authority_confirmed":true})),
    );
    assert_eq!(s, 403);
    assert_eq!(v["error"]["code"], "human_owner_required");
    assert_eq!(
        f.owner("/owner/device/lookup", Some(json!({"user_code":code})))
            .1["request"]["status"],
        "pending"
    );
    assert_eq!(
        f.agent("/owner/revoke", Some(json!({"id":&token[5..37]})))
            .0,
        200
    );
    assert_eq!(f.cli(&token).0, 401);
    // The App's own execution identity (what external tokens act as) cannot manage.
    for path in ["/owner/state", "/owner/audit"] {
        assert_eq!(
            f.call(
                "GET",
                path,
                &[("x-treer-identity-token", "self_agent")],
                None
            )
            .0,
            401
        );
    }
    // Authorization must be exactly the identity token, never another credential.
    assert_eq!(
        f.call(
            "GET",
            "/owner/state",
            &[
                ("x-treer-identity-token", "agent"),
                ("authorization", "Bearer other")
            ],
            None
        )
        .0,
        401
    );
    // Agents still cannot send foreign browser origins.
    assert_eq!(
        f.call(
            "POST",
            "/owner/tokens",
            &[
                ("x-treer-identity-token", "agent"),
                ("origin", "https://evil.example")
            ],
            Some(json!({"name":"x","days":30,"authority_confirmed":true}))
        )
        .0,
        401
    );
    // A human browser session still needs Origin + Fetch Metadata + CSRF for mutations.
    assert_eq!(
        f.call(
            "POST",
            "/owner/tokens",
            &[("x-treer-identity-token", "owner")],
            Some(json!({"name":"x","days":30,"authority_confirmed":true}))
        )
        .0,
        401
    );
}
#[test]
fn missing_endpoint_inbox_capability_is_reported_and_rechecked() {
    let f = Fixture::build(true, true);
    let full = f.create();
    let key = f.register("alice");
    f.enable();
    let (s, v) = f.external(&key, "GET", "/v1/inbox", None);
    assert_eq!(s, 503, "{v}");
    assert_eq!(v["error"]["code"], "inbox_unsupported");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("message.endpoint-inbox.v1"));
    assert_eq!(
        f.external(
            &key,
            "POST",
            "/v1/inbox/ack",
            Some(json!({"delivery_ids":["dlv_1"],"operation_id":"op_1"}))
        )
        .0,
        503
    );
    assert_eq!(f.backend.calls.load(Ordering::Relaxed), 0);
    // Everything else keeps working.
    assert_eq!(f.cli(&full).0, 200);
    let (_, state) = f.owner("/owner/state", None);
    assert_eq!(state["readiness"]["inbox"]["available"], false);
    assert_eq!(state["readiness"]["cli"]["available"], true);
    assert_eq!(state["readiness"]["cli"]["version"], "0.2.0");
    let (_, info) = f.owner("/owner/integration", None);
    assert!(info["blockers"]
        .as_array()
        .unwrap()
        .contains(&json!("inbox_unsupported")));
    // After a Treer upgrade the next request sees the capability without restart.
    f.backend
        .no_inbox_capability
        .store(false, Ordering::Relaxed);
    let (s, v) = f.external(&key, "GET", "/v1/inbox", None);
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        f.owner("/owner/state", None).1["readiness"]["inbox"]["available"],
        true
    );
}
#[test]
fn cli_is_checked_by_version_not_hash() {
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    assert_eq!(f.cli(&token).0, 200);
    // A Treer update replacing the binary in place keeps the gateway working.
    let path = f.root.join("treer");
    std::fs::write(&path, CLI.replace("treer 0.2.0", "treer 0.3.1")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(f.cli(&token).1["result"]["exit_code"], 0);
    // A binary that does not identify as the treer CLI is refused with a reason.
    let d = tempfile::tempdir().unwrap();
    let bogus = d.path().join("not-treer");
    std::fs::write(&bogus, "#!/bin/sh\necho hello\n").unwrap();
    std::fs::set_permissions(&bogus, std::fs::Permissions::from_mode(0o700)).unwrap();
    let g = super::CliGateway::new(bogus);
    assert!(g.status().unwrap_err().contains("did not identify"));
    let g = super::CliGateway::new("/nonexistent/treer".into());
    assert!(g.status().is_err());
    // Without treer_cli the gateway reports itself unavailable.
    let f = Fixture::build(false, false);
    let token = f.create();
    f.enable();
    let (s, v) = f.cli(&token);
    assert_eq!(s, 503);
    assert_eq!(v["result"]["error"]["code"], "cli_unavailable");
    assert_eq!(
        f.owner("/owner/state", None).1["readiness"]["cli"]["available"],
        false
    );
}
#[test]
fn tokens_survive_config_edits_and_restart_but_not_identity_change() {
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    let mut config = f.config.clone();
    config.public_origin = "https://new-data.example/".parse().unwrap();
    config.console_url = "https://console.example/elsewhere/".parse().unwrap();
    let state = config.state.clone();
    let Fixture {
        _dir,
        root,
        app,
        backend,
        public,
        private,
        ..
    } = f;
    drop((public, private));
    drop(app);
    let (controller, proxy) = backend_services(&backend);
    let mut moved = config_for(&root, &controller, &proxy, Some(root.join("treer")));
    moved.public_origin = config.public_origin;
    moved.state = state;
    let app = App::open(moved.clone(), WORKLOAD.into()).unwrap();
    let a = app.clone();
    let public = Listener::start(move |s| a.accept(s));
    let (s, v) = call(
        public.address,
        "POST",
        "/v1/cli",
        &[("authorization", &format!("Bearer {token}"))],
        Some(json!({"argv":["agent","ls"]})),
    );
    assert_eq!(s, 200, "{v}");
    drop(public);
    drop(app);
    let mut other = moved.clone();
    other.app = "app_other".into();
    assert!(App::open(other, WORKLOAD.into()).is_err());
}
#[test]
fn config_loads_legacy_fields_and_rejects_unknown_ones() {
    let f = Fixture::new();
    let mut doc = json!({
        "workspace":"ws_fixture","app":"app_fixture","service":"svc_fixture","agent":OWN,
        "machine":"machine_fixture","controller":f.config.controller,"proxy":f.config.proxy,
        "public_origin":"https://data.example/","owner_origin":"https://external.example/",
        "console_origin":"https://console.example/","tunnel_origin":"https://proxy.example/",
        "console_url":"https://console.example/workspaces/ws_fixture/",
        "listen":"127.0.0.1:0","owner_listen":"127.0.0.1:0","state":f.config.state,
        "policy_acceptance_sha256":null,"prompt_review_sha256":"a".repeat(64)
    });
    let path = f.root.join("config.json");
    let write = |doc: &Value| {
        std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    write(&doc);
    assert!(Config::load(&path).unwrap().treer_cli.is_none());
    doc["treer_cli"] = json!("/usr/local/bin/treer");
    write(&doc);
    assert!(Config::load(&path).unwrap().treer_cli.is_some());
    doc["treer_cli"] = json!("relative/treer");
    write(&doc);
    assert!(Config::load(&path).is_err());
    doc["treer_cli"] = Value::Null;
    doc["typo_field"] = json!(1);
    write(&doc);
    assert!(Config::load(&path).is_err());
}
#[test]
fn fresh_owner_verification_each_time_and_csrf_never_falls_back() {
    let f = Fixture::new();
    for token in [
        "member",
        "self_agent",
        "expired",
        "oauth",
        "wrong_audience",
        "wrong_workspace",
        "invalid",
    ] {
        let (s, _) = f.call(
            "GET",
            "/owner/state",
            &[
                ("x-treer-identity-token", token),
                ("cookie", "apparently_valid"),
            ],
            None,
        );
        assert_eq!(s, 401, "{token}");
    }
    assert_eq!(f.owner("/owner/state", None).0, 200);
    assert_eq!(f.owner("/owner/state", None).0, 200);
    assert_eq!(f.backend.verifies.load(Ordering::Relaxed), 9);
    for origin in ["null", "https://evil.example"] {
        assert_eq!(
            f.call(
                "POST",
                "/owner/tokens",
                &[
                    ("x-treer-identity-token", "owner"),
                    ("origin", origin),
                    ("sec-fetch-site", "same-origin"),
                    ("x-external-agent-csrf", "1")
                ],
                Some(json!({"name":"bad","days":30,"authority_confirmed":true}))
            )
            .0,
            401
        );
    }
    assert_eq!(f.backend.verifies.load(Ordering::Relaxed), 9);
    let token = f.create();
    assert_eq!(
        f.call(
            "GET",
            "/owner/state",
            &[
                ("authorization", &format!("Bearer {token}")),
                ("x-treer-identity-token", "owner")
            ],
            None
        )
        .0,
        401
    );
}
#[test]
fn expiry_revoke_and_containment_during_blocked_cli() {
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    let addr = f.public.address;
    let t = token.clone();
    let pending = thread::spawn(move || {
        call(
            addr,
            "POST",
            "/v1/cli",
            &[("authorization", &format!("Bearer {t}"))],
            Some(json!({"argv":["block"]})),
        )
    });
    let end = Instant::now() + Duration::from_secs(3);
    while !f.root.join("entered").exists() && Instant::now() < end {
        thread::sleep(Duration::from_millis(2));
    }
    assert!(f.root.join("entered").exists());
    let start = Instant::now();
    assert_eq!(
        f.owner("/owner/revoke", Some(json!({"id":&token[5..37]})))
            .0,
        200
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    std::fs::write(f.root.join("release"), b"").unwrap();
    assert_eq!(pending.join().unwrap().0, 200);
    assert_eq!(f.cli(&token).0, 401);
    let t = f.create();
    {
        let store = f.app.store.lock().unwrap();
        store
            .test_db()
            .execute(
                "UPDATE tokens SET expires=created+1 WHERE id=?1",
                [&t[5..37]],
            )
            .unwrap();
        store
            .test_db()
            .execute("UPDATE settings SET high_water=?1", [now().unwrap() + 2])
            .unwrap();
    }
    assert_eq!(f.cli(&t).0, 401);
}
#[test]
fn hostile_routes_fields_and_browser_auth_do_not_reach_cli() {
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    for path in [
        "/v1/agents",
        "/v1/agents/ag_22222222222222222222222222222222/stop",
        "/v1/workspaces/ws_other/agents",
    ] {
        assert_eq!(f.external(&token, "POST", path, Some(json!({}))).0, 404);
    }
    assert_eq!(
        f.external(
            &token,
            "POST",
            "/v1/cli?token=secret",
            Some(json!({"argv":["agent","ls"]}))
        )
        .0,
        400
    );
    for field in ["sender", "workspace", "url", "env", "headers"] {
        assert_eq!(
            f.external(
                &token,
                "POST",
                "/v1/cli",
                Some(json!({"argv":["agent","ls"],field:"bad"}))
            )
            .0,
            400
        );
    }
    for header in [
        ("origin", "https://external.example"),
        ("sec-fetch-site", "none"),
        ("cookie", "secret"),
        ("x-treer-workload-credential", "forged"),
    ] {
        assert_eq!(
            f.call(
                "POST",
                "/v1/cli",
                &[("authorization", &format!("Bearer {token}")), header],
                Some(json!({"argv":["agent","ls"]}))
            )
            .0,
            401
        );
    }
    assert_eq!(f.cli_calls(), 0);
}

#[test]
fn failure_budget_does_not_lock_out_valid_token_or_write_state() {
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    let before = f.app.store.lock().unwrap().test_db().total_changes();
    for _ in 0..25 {
        let (s, _) = f.cli("invalid");
        assert!(matches!(s, 401 | 429));
    }
    assert_eq!(
        f.app.store.lock().unwrap().test_db().total_changes(),
        before
    );
    assert_eq!(f.cli(&token).0, 200);
    for _ in 1..60 {
        assert_eq!(f.cli(&token).0, 200);
    }
    assert_eq!(f.cli(&token).0, 429);
    assert_eq!(
        f.owner("/owner/revoke", Some(json!({"id":&token[5..37]})))
            .0,
        200
    );
}
#[test]
fn lost_or_oversized_inbox_response_is_unknown_and_never_retried() {
    let f = Fixture::new();
    let key = f.register("alice");
    f.enable();
    f.backend.lost.store(true, Ordering::Relaxed);
    let (s, v) = f.external(
        &key,
        "POST",
        "/v1/inbox/ack",
        Some(json!({"delivery_ids":["dlv_1"],"operation_id":"op_1"})),
    );
    assert_eq!(s, 502);
    assert_eq!(v["result"]["error"]["code"], "outcome_unknown");
    assert_eq!(f.backend.calls.load(Ordering::Relaxed), 1);
    f.backend.lost.store(false, Ordering::Relaxed);
    f.backend.oversize.store(true, Ordering::Relaxed);
    assert_eq!(f.external(&key, "GET", "/v1/inbox", None).0, 502);
    assert_eq!(f.backend.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn framing_limits_duplicate_auth_slow_headers_and_negotiation() {
    let f = Fixture::new();
    for headers in [
        "Content-Length: 0\r\nContent-Length: 0\r\n",
        "Content-Length: 0\r\nTransfer-Encoding: chunked\r\n",
        "Authorization: Bearer one\r\nAuthorization: Bearer two\r\n",
        "Content-Length: 65537\r\n",
    ] {
        let mut s = TcpStream::connect(f.public.address).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(format!("POST /v1/cli HTTP/1.1\r\nHost: fixture\r\n{headers}\r\n").as_bytes())
            .unwrap();
        let mut b = String::new();
        s.read_to_string(&mut b).unwrap();
        assert!(b.starts_with("HTTP/1.1 400"));
    }
    let mut s = TcpStream::connect(f.public.address).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let start = Instant::now();
    s.write_all(b"GET / HTTP/1.1\r\nHost:").unwrap();
    let mut b = String::new();
    s.read_to_string(&mut b).unwrap();
    assert!(b.starts_with("HTTP/1.1 400"));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(f.call("GET", "/", &[], None).1["text"]
        .as_str()
        .unwrap()
        .starts_with("# External"));
    assert!(
        f.call("GET", "/", &[("accept", "text/html")], None).1["text"]
            .as_str()
            .unwrap()
            .starts_with("<!doctype")
    );
}

#[test]
fn configured_destinations_and_private_config_fail_closed() {
    let f = Fixture::new();
    let mut c = f.config.clone();
    c.controller = "http://192.0.2.1/".parse().unwrap();
    assert!(c.validate().is_err());
    c = f.config.clone();
    c.proxy = "http://example.com/".parse().unwrap();
    assert!(c.validate().is_err());
    c = f.config.clone();
    c.console_url = "https://evil.example/".parse().unwrap();
    assert!(c.validate().is_err());
    c = f.config.clone();
    c.agent = "../../victim".into();
    assert!(c.validate().is_err());
    assert!(App::open(f.config.clone(), String::new()).is_err());
    c = f.config.clone();
    c.workspace = "ws_other".into();
    assert!(App::open(c, WORKLOAD.into()).is_err());
    let path = f._dir.path().join("config.json");
    let doc = json!({
        "workspace":"ws_fixture","app":"app_fixture","service":"svc_fixture","agent":OWN,
        "machine":"machine_fixture","controller":f.config.controller,"proxy":f.config.proxy,
        "public_origin":"https://data.example/","owner_origin":"https://external.example/",
        "console_origin":"https://console.example/","tunnel_origin":"https://proxy.example/",
        "console_url":"https://console.example/workspaces/ws_fixture/",
        "listen":"127.0.0.1:0","owner_listen":"127.0.0.1:0","state":f.config.state
    });
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Config::load(&path).is_ok());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Config::load(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = f._dir.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(Config::load(&link).is_err());
}
#[test]
fn wrong_credential_flood_does_not_starve_owner_revoke() {
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    let mut wrong = token.clone();
    let last = wrong.pop().unwrap();
    wrong.push(if last == '0' { '1' } else { '0' });
    let mut threads = Vec::new();
    for _ in 0..4 {
        let addr = f.public.address;
        let wrong = wrong.clone();
        threads.push(thread::spawn(move || {
            for _ in 0..30 {
                let (s, _) = call(
                    addr,
                    "POST",
                    "/v1/cli",
                    &[("authorization", &format!("Bearer {wrong}"))],
                    Some(json!({"argv":["agent","ls"]})),
                );
                assert!(matches!(s, 401 | 429));
            }
        }));
    }
    let start = Instant::now();
    assert_eq!(
        f.owner("/owner/revoke", Some(json!({"id":&token[5..37]})))
            .0,
        200
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(f.cli_calls(), 0);
}

#[test]
fn cheap_missing_credentials_are_rejected_before_operation_slots() {
    let f = Fixture::new();
    f.app.test_fill_slots(true);
    let before = f.app.store.lock().unwrap().test_db().total_changes();
    for _ in 0..25 {
        assert_eq!(f.call("GET", "/owner/state", &[], None).0, 401);
        assert_eq!(
            f.call(
                "GET",
                "/owner/state",
                &[("x-treer-identity-token", "")],
                None
            )
            .0,
            401
        );
        assert_eq!(f.call("POST", "/v1/cli", &[], None).0, 401);
        assert_eq!(f.cli("not-a-token").0, 401);
    }
    f.app.test_fill_slots(false);
    assert_eq!(f.backend.verifies.load(Ordering::Relaxed), 0);
    assert_eq!(f.cli_calls(), 0);
    assert_eq!(
        f.app.store.lock().unwrap().test_db().total_changes(),
        before
    );
}
#[test]
fn unauthenticated_owner_read_flood_preserves_revoke_lane() {
    let f = Fixture::new();
    let token = f.create();
    f.backend.slow_invalid_owner.store(true, Ordering::Relaxed);
    let before = f.backend.verifies.load(Ordering::Relaxed);
    let mut flood = Vec::new();
    for _ in 0..4 {
        let addr = f.private.address;
        flood.push(thread::spawn(move || {
            for _ in 0..12 {
                let (status, _) = call(
                    addr,
                    "GET",
                    "/owner/state",
                    &[("x-treer-identity-token", "invalid")],
                    None,
                );
                assert!(matches!(status, 401 | 429));
                assert_eq!(call(addr, "GET", "/owner/audit", &[], None).0, 401);
            }
        }));
    }
    let wait = Instant::now();
    while f.backend.verifies.load(Ordering::Relaxed) < before + 2 {
        assert!(wait.elapsed() < Duration::from_secs(2));
        thread::yield_now();
    }
    let start = Instant::now();
    assert_eq!(
        f.owner("/owner/revoke", Some(json!({"id":&token[5..37]})))
            .0,
        200
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    for t in flood {
        t.join().unwrap();
    }
    assert_eq!(f.backend.calls.load(Ordering::Relaxed), 0);
    // The owner remains freshly verified, not authorized by a scheduling/auth cache.
    assert!(f.backend.verifies.load(Ordering::Relaxed) > before + 2);
    assert!(f
        .app
        .store
        .lock()
        .unwrap()
        .test_db()
        .query_row(
            "SELECT revoked_at IS NOT NULL FROM tokens WHERE id=?1",
            [&token[5..37]],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn public_parser_saturation_cannot_take_management_connections() {
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    let mut stalled = Vec::new();
    for _ in 0..24 {
        let mut stream = TcpStream::connect(f.public.address).unwrap();
        stream
            .write_all(b"GET /owner/audit HTTP/1.1\r\nHost: fixture\r\n")
            .unwrap();
        stalled.push(stream);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while f.app.test_public_connections() != 24 {
        assert!(Instant::now() < deadline, "public connections never filled");
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        f.owner("/owner/revoke", Some(json!({"id":&token[5..37]})))
            .0,
        200
    );
    assert_eq!(f.app.test_public_connections(), 24);
    drop(stalled);
    let deadline = Instant::now() + Duration::from_secs(2);
    while f.app.test_public_connections() != 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(f.cli(&token).0, 401);
}
#[test]
fn socket_role_cannot_be_overridden_by_headers_or_credentials() {
    let f = Fixture::new();
    let token = f.create();
    let headers = [
        ("x-treer-identity-token", "owner"),
        ("origin", "https://external.example"),
        ("sec-fetch-site", "same-origin"),
        ("x-external-agent-csrf", "1"),
    ];
    for (method, path, body) in [
        ("GET", "/owner/state", None),
        ("GET", "/owner/audit", None),
        ("POST", "/owner/revoke", Some(json!({"id":&token[5..37]}))),
        ("POST", "/owner/activation", Some(json!({"enabled":false}))),
        ("POST", "/owner/tokens", Some(json!({"name":"forged"}))),
    ] {
        assert_eq!(call(f.public.address, method, path, &headers, body).0, 404);
    }
    assert_eq!(
        call(
            f.private.address,
            "POST",
            "/v1/cli",
            &[("authorization", &format!("Bearer {token}"))],
            Some(json!({"argv":["agent","ls"]}))
        )
        .0,
        404
    );
    assert_eq!(f.owner("/owner/state", None).0, 200);
    let data_origin_headers = [
        ("x-treer-identity-token", "owner"),
        ("origin", "https://data.example"),
        ("sec-fetch-site", "same-origin"),
        ("x-external-agent-csrf", "1"),
    ];
    assert_eq!(
        call(
            f.private.address,
            "POST",
            "/owner/revoke",
            &data_origin_headers,
            Some(json!({"id":&token[5..37]}))
        )
        .0,
        401
    );
    let mut c = f.config.clone();
    c.owner_origin = c.public_origin.clone();
    assert!(c.validate().is_err());
    c.owner_origin = f.config.owner_origin.clone();
    c.owner_listen = "0.0.0.0:8888".parse().unwrap();
    assert!(c.validate().is_err());
    c.owner_listen = "127.0.0.1:8888".parse().unwrap();
    c.listen = c.owner_listen;
    assert!(c.validate().is_err());
}

#[test]
fn ingress_client_ip_header_is_compatible_but_never_authority() {
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    for value in ["192.0.2.1", "arbitrary-untrusted-value"] {
        assert_eq!(
            f.call(
                "POST",
                "/v1/cli",
                &[
                    ("authorization", &format!("Bearer {token}")),
                    ("x-treer-client-ip", value)
                ],
                Some(json!({"argv":["agent","ls"]}))
            )
            .0,
            200
        );
        assert_eq!(
            f.call(
                "POST",
                "/v1/cli",
                &[("x-treer-client-ip", value)],
                Some(json!({"argv":["agent","ls"]}))
            )
            .0,
            401
        );
    }
    assert_eq!(
        f.call(
            "POST",
            "/v1/cli",
            &[
                ("authorization", &format!("Bearer {token}")),
                ("x-treer-client-ip", "192.0.2.1"),
                ("x-treer-agent-id", OWN)
            ],
            Some(json!({"argv":["agent","ls"]}))
        )
        .0,
        401
    );
    assert_eq!(f.cli_calls(), 2);
}

#[test]
fn console_iframe_root_accepts_only_non_authoritative_appearance_queries() {
    let f = Fixture::new();
    let headers = [("accept", "text/html")];
    for query in [
        "theme=dark",
        "theme=light&look=b&lang=zh&embed_origin=https%3A%2F%2Fconsole.example",
        "embed_origin=https%3A%2F%2Fevil.example",
    ] {
        // Even an untrusted appearance hint cannot change the fixed response CSP.
        let result = f.call("GET", &format!("/?{query}"), &headers, None);
        assert_eq!(result.0, 200);
    }
    for path in [
        "/?theme=dark&theme=light",
        "/?unknown=x",
        "/?embed_origin=javascript%3Aevil",
        "/owner/state?theme=dark",
        "/v1/cli?theme=dark",
    ] {
        assert_eq!(f.call("GET", path, &headers, None).0, 400);
    }
    assert_eq!(f.call("GET", "/owner/state", &[], None).0, 401);
    assert_eq!(f.owner("/owner/state?theme=dark", None).0, 400);
    let mut stream = TcpStream::connect(f.private.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(b"GET /?theme=dark&embed_origin=https%3A%2F%2Fevil.example HTTP/1.1\r\nHost: fixture\r\nAccept: text/html\r\n\r\n").unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let response = String::from_utf8(bytes).unwrap();
    let headers = response.split("\r\n\r\n").next().unwrap();
    assert!(headers.starts_with("HTTP/1.1 200"));
    assert!(headers.contains("frame-ancestors https://console.example https://proxy.example;"));
    assert!(!headers.contains("evil.example"));
}

#[test]
fn owner_integration_and_readable_skill_are_secret_free_and_do_not_enable() {
    let f = Fixture::new();
    let (status, document) = f.call("GET", "/SKILL.md", &[], None);
    assert_eq!(status, 200);
    let document = document["text"].as_str().unwrap();
    assert!(document.contains("https://data.example/"));
    assert!(document.contains("TREER_EXTERNAL_TOKEN_FILE"));
    assert!(!document.contains(WORKLOAD));
    assert!(!document.contains("{{"));
    assert_eq!(f.call("GET", "/owner/integration", &[], None).0, 401);
    for token in [
        "member",
        "self_agent",
        "expired",
        "wrong_audience",
        "wrong_workspace",
    ] {
        assert_eq!(
            f.call(
                "GET",
                "/owner/integration",
                &[("x-treer-identity-token", token)],
                None
            )
            .0,
            401
        );
    }
    let (status, info) = f.owner("/owner/integration", None);
    assert_eq!(status, 200);
    assert_eq!(info["identity"]["app"], f.config.app);
    assert_eq!(info["data_base_url"], "https://data.example/");
    assert_eq!(info["enabled"], false);
    assert_eq!(info["reachability"], "configured_https_unverified");
    assert!(info["blockers"]
        .as_array()
        .unwrap()
        .contains(&json!("disabled")));
    assert!(info["blockers"]
        .as_array()
        .unwrap()
        .contains(&json!("data_mapping_not_verified")));
    let before = f.backend.verifies.load(Ordering::Relaxed);
    assert_eq!(f.owner("/owner/integration", None).0, 200);
    assert_eq!(f.backend.verifies.load(Ordering::Relaxed), before + 1);
    assert_eq!(f.backend.calls.load(Ordering::Relaxed), 0);
    assert_eq!(f.owner("/owner/state", None).1["tokens"], json!([]));
    assert_eq!(f.owner("/owner/audit", None).1["requests"], json!([]));
    assert_eq!(f.owner("/owner/integration?x=1", None).0, 400);
    let mut local = f.config.clone();
    local.public_origin = "http://127.0.0.1:62921/".parse().unwrap();
    let info = super::integration::view(&local, false);
    assert_eq!(info["reachability"], "loopback_only");
    assert_eq!(info["activation_available"], true);
    assert!(info["blockers"]
        .as_array()
        .unwrap()
        .contains(&json!("loopback_only")));
    assert!(!info["blockers"]
        .as_array()
        .unwrap()
        .contains(&json!("policy_acceptance_missing")));
    assert!(info["skill_markdown"]
        .as_str()
        .unwrap()
        .contains("http://127.0.0.1:62921/"));
    assert!(!info.to_string().contains(WORKLOAD));
}

#[test]
fn copied_skill_python_reads_private_file_and_discovers_without_token_output() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let f = Fixture::new();
    let token = f.create();
    f.enable();
    let path = f._dir.path().join("agent-token");
    std::fs::write(&path, &token).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = f.config.clone();
    config.public_origin = format!("http://{}/", f.public.address).parse().unwrap();
    let manual = super::integration::manual(&config);
    let script = manual
        .split("python3 - <<'PY'\n")
        .nth(1)
        .unwrap()
        .split("\nPY\n")
        .next()
        .unwrap();
    let mut child = Command::new("/usr/bin/python3")
        .args(["-I", "-B", "-"])
        .env_clear()
        .env("TREER_EXTERNAL_TOKEN_FILE", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&token));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"], 0);
    assert_eq!(f.cli_calls(), 1);
}

#[test]
fn cli_gateway_tcp_argv_stdin_exit_auth_and_bounds() {
    let f = Fixture::new();
    let token = f.create();
    let req = json!({"argv":["agent","ls","--","literal ; $(echo nope)"],"stdin":"envelope\n"});
    assert_eq!(
        f.external(&token, "POST", "/v1/cli", Some(req.clone())).0,
        503
    );
    f.enable();
    assert_eq!(
        f.external("bad", "POST", "/v1/cli", Some(req.clone())).0,
        401
    );
    let (s, v) = f.external(&token, "POST", "/v1/cli", Some(req));
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["result"]["exit_code"], 0);
    let out: Value = serde_json::from_str(v["result"]["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(
        out["argv"],
        json!(["agent", "ls", "--", "literal ; $(echo nope)"])
    );
    assert_eq!(out["stdin"], "envelope\n");
    assert_eq!(out["agent"], OWN);
    assert_eq!(out["mode"], "agent");
    let keys = out["keys"].as_array().unwrap();
    assert!(
        keys.iter()
            .all(|k| k.as_str().unwrap().starts_with("TREER_")
                || k == "LC_CTYPE"
                || matches!(
                    k.as_str(),
                    Some(
                        "__CF_USER_TEXT_ENCODING"
                            | "CPATH"
                            | "LIBRARY_PATH"
                            | "MANPATH"
                            | "SDKROOT"
                    )
                )),
        "{keys:?}"
    );
    assert!(!keys.contains(&json!("TREER_EXTERNAL_TOKEN")));
    assert!(!keys.contains(&json!("TREER_OPERATOR_CREDENTIAL")));
    assert_eq!(v["result"]["stderr"], "fixture-stderr");
    assert_eq!(v["result"]["outcome_uncertain"], false);
    for argv in [
        json!(["--format", "json", "--url", "http://evil", "agent", "ls"]),
        json!(["agent", "ls"]),
    ] {
        let mut bad = json!({"argv":argv});
        if bad["argv"][0] == "agent" {
            bad["env"] = json!({"TREER_OPERATOR_CREDENTIAL":"no"});
        }
        assert_eq!(f.external(&token, "POST", "/v1/cli", Some(bad)).0, 400);
    }
    for argv in [
        json!(["agent", "attach", TARGET]),
        json!(["machine", "get", "secret"]),
        json!(["agent", "read", TARGET, "--follow"]),
        json!(["message", "send", "--body-file", "private.db"]),
        json!([
            "--format",
            "json",
            "message",
            "send",
            "--external-source-file=private.json"
        ]),
    ] {
        let (s, v) = f.external(&token, "POST", "/v1/cli", Some(json!({"argv":argv})));
        assert_eq!(s, 400);
        assert_eq!(v["error"]["code"], "unsupported_cli_mode");
    }
    for argv in [
        json!(["message", "send", "--body-file", "-"]),
        json!(["message", "send", "--external-source-file=-"]),
        json!(["agent", "exec", "--", "--body-file", "private.db"]),
    ] {
        let (s, v) = f.external(
            &token,
            "POST",
            "/v1/cli",
            Some(json!({"argv":argv,"stdin":"fixture"})),
        );
        assert_eq!(s, 200);
        assert_eq!(v["result"]["exit_code"], 0);
    }
    let (_, v) = f.external(&token, "POST", "/v1/cli", Some(json!({"argv":["nonzero"]})));
    assert_eq!(v["result"]["exit_code"], 7);
    assert_eq!(v["result"]["outcome_uncertain"], false);
    let (_, v) = f.external(
        &token,
        "POST",
        "/v1/cli",
        Some(json!({"argv":["sleep"],"timeout_ms":50})),
    );
    assert_eq!(v["result"]["timed_out"], true);
    assert_eq!(v["result"]["outcome_uncertain"], true);
    let (_, v) = f.external(&token, "POST", "/v1/cli", Some(json!({"argv":["large"]})));
    assert_eq!(v["result"]["truncated"], true);
    assert_eq!(v["result"]["outcome_uncertain"], true);
    assert!(v["result"]["stdout"].as_str().unwrap().len() <= 2 * 1024 * 1024);
    let (_, v) = f.external(&token, "POST", "/v1/cli", Some(json!({"argv":["fork"]})));
    assert_eq!(v["result"]["exit_code"], 0); // descendant-held pipes cannot hang the request
    let (_, audit) = f.owner("/owner/audit", None);
    let text = audit.to_string();
    assert!(!text.contains("literal"));
    assert!(!text.contains("envelope"));
    assert!(text.contains("outcome_unknown"));
    // Management remains available while native CLI is running outside the Store mutex.
    let address = f.public.address;
    let credential = token.clone();
    let running = thread::spawn(move || {
        call(
            address,
            "POST",
            "/v1/cli",
            &[("authorization", &format!("Bearer {credential}"))],
            Some(json!({"argv":["sleep"],"timeout_ms":300})),
        )
    });
    thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    assert_eq!(
        f.owner("/owner/revoke", Some(json!({"id":&token[5..37]})))
            .0,
        200
    );
    assert!(started.elapsed() < Duration::from_millis(200));
    assert_eq!(running.join().unwrap().1["result"]["timed_out"], true);
    assert_eq!(
        f.external(
            &token,
            "POST",
            "/v1/cli",
            Some(json!({"argv":["agent","ls"]}))
        )
        .0,
        401
    );
}

#[test]
fn external_inbox_tcp_registration_poll_is_nonconsuming_and_ack_is_scoped() {
    let f = Fixture::new();
    let full = f.create();
    f.enable();
    let mut keys = Vec::new();
    for key in ["alice", "bob"] {
        let (status, v) = f.owner(
            "/owner/external-agents",
            Some(json!({"key":key,"name":key,"days":30,"authority_confirmed":true})),
        );
        assert_eq!(status, 201, "{v}");
        assert_eq!(v["agent"]["address"], format!("{OWN}/{key}"));
        keys.push(v["plaintext"].as_str().unwrap().to_owned());
        f.backend.inbox.lock().unwrap().push(json!({"delivery_id":format!("dlv_{key}"),"recipient":{"id":OWN,"app_endpoint":key},"message":{"workspace_id":"ws_fixture","kind":if key=="alice" {"prompt"}else{"message"},"body":"synthetic"}}));
    }
    f.backend.inbox.lock().unwrap()[1]["message"]
        .as_object_mut()
        .unwrap()
        .remove("kind");
    let before = f.backend.calls.load(Ordering::Relaxed);
    for argv in [
        json!(["message", "receive"]),
        json!(["--format", "json", "message", "get", "msg_other"]),
        json!(["message", "ack", "dlv_bob"]),
    ] {
        assert_eq!(
            f.external(&keys[0], "POST", "/v1/cli", Some(json!({"argv":argv})))
                .0,
            403
        );
    }
    assert_eq!(f.backend.calls.load(Ordering::Relaxed), before);
    assert_eq!(
        f.external(
            &keys[0],
            "POST",
            "/v1/cli",
            Some(json!({"argv":["whoami"]}))
        )
        .0,
        200
    );
    let headers = [("x-treer-identity-token", "owner")];
    let (s, v) = call(f.private.address, "GET", "/api/tree?path=", &headers, None);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        call(f.public.address, "GET", "/api/tree?path=", &headers, None).0,
        404
    );
    assert_eq!(f.external(&full, "GET", "/v1/inbox", None).0, 400);
    for (key, token) in ["alice", "bob"].iter().zip(&keys) {
        for _ in 0..2 {
            let (s, v) = f.external(token, "GET", "/v1/inbox?limit=10", None);
            assert_eq!(s, 200, "{v}");
            assert_eq!(v["result"]["deliveries"].as_array().unwrap().len(), 1);
            assert_eq!(
                v["result"]["deliveries"][0]["recipient"]["app_endpoint"],
                *key
            );
            // The top-level cursor pages forward; an empty page carries no cursor.
            let cursor = v["result"]["next_cursor"].as_str().unwrap();
            let (s, next) = f.external(token, "GET", &format!("/v1/inbox?cursor={cursor}"), None);
            assert_eq!(s, 200, "{next}");
            assert!(next["result"]["deliveries"].as_array().unwrap().is_empty());
            assert!(next["result"].get("next_cursor").is_none());
        }
        assert_eq!(f.cli(token).0, 200);
        assert_eq!(f.external(token, "GET", "/v1/external-agents", None).0, 401);
    }
    let (s, v) = f.external(
        &keys[0],
        "POST",
        "/v1/inbox/ack",
        Some(json!({"delivery_ids":["dlv_bob"],"operation_id":"op_foreign"})),
    );
    assert_eq!(s, 404);
    assert_eq!(
        v["result"],
        json!({"error":{"code":"message_delivery_not_found"}})
    );
    for _ in 0..2 {
        assert_eq!(
            f.external(
                &keys[0],
                "POST",
                "/v1/inbox/ack",
                Some(json!({"delivery_ids":["dlv_alice"],"operation_id":"op_alice"}))
            )
            .0,
            200
        );
    }
    assert!(
        f.external(&keys[0], "GET", "/v1/inbox", None).1["result"]["deliveries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.external(&keys[1], "GET", "/v1/inbox", None).1["result"]["deliveries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.external(&keys[1], "GET", "/v1/inbox?limit=0", None).0,
        400
    );
    assert_eq!(
        f.external(&keys[1], "GET", "/v1/inbox?app_endpoint=alice", None)
            .0,
        400
    );
    let (s, v) = f.owner(
        "/owner/external-agents/tokens",
        Some(json!({"key":"alice","name":"replacement","days":30,"authority_confirmed":true})),
    );
    assert_eq!(s, 201, "{v}");
    assert_eq!(
        f.external(v["plaintext"].as_str().unwrap(), "GET", "/v1/inbox", None)
            .0,
        200
    );
}

#[test]
fn external_inbox_creator_bound_revoke_is_idempotent_and_preserves_user_keys() {
    let f = Fixture::new();
    let creator = f.create();
    let other = f.create();
    f.enable();
    let (s, v) = f.external(
        &creator,
        "POST",
        "/v1/external-agents",
        Some(json!({"key":"custody","name":"custody","days":30,"authority_confirmed":true})),
    );
    assert_eq!(s, 201, "{v}");
    let child = v["result"]["plaintext"].as_str().unwrap();
    let body = json!({"id":v["result"]["token"]["id"]});
    assert_eq!(
        f.external(
            child,
            "POST",
            "/v1/external-agents/revoke-token",
            Some(body.clone())
        )
        .0,
        401
    );
    assert_eq!(
        f.external(
            &other,
            "POST",
            "/v1/external-agents/revoke-token",
            Some(body.clone())
        )
        .0,
        403
    );
    assert_eq!(
        f.external(
            &creator,
            "POST",
            "/v1/external-agents/revoke-token",
            Some(json!({"id":&other[5..37]}))
        )
        .0,
        403
    );
    assert_eq!(
        f.external(
            &creator,
            "POST",
            "/v1/external-agents/revoke-token",
            Some(json!({"id":"00000000000000000000000000000000"}))
        )
        .0,
        403
    );
    for _ in 0..2 {
        assert_eq!(
            f.external(
                &creator,
                "POST",
                "/v1/external-agents/revoke-token",
                Some(body.clone())
            )
            .0,
            200
        );
    }
    assert_eq!(f.cli(child).0, 401);
    for key in [&creator, &other] {
        assert_eq!(f.cli(key).0, 200);
    }
    assert_eq!(
        f.external(&creator, "GET", "/v1/external-agents", None).1["result"]["agents"][0]["key"],
        "custody"
    );
}

#[test]
fn device_authorization_tcp_uses_fresh_owner_proof_and_concurrent_once_claim() {
    let f = Fixture::new();
    let old = f.create();
    f.enable();
    let (status, request) = f.call(
        "POST",
        "/v1/device/request",
        &[],
        Some(json!({"key":"device","name":"Device","expires_in":600})),
    );
    assert_eq!(status, 201, "{request}");
    assert_eq!(request["verification_uri"], "https://external.example/");
    let code = request["user_code"].as_str().unwrap();
    let secret = request["device_code"].as_str().unwrap();
    assert_eq!(
        f.call(
            "POST",
            "/owner/device/lookup",
            &[],
            Some(json!({"user_code":code}))
        )
        .0,
        401
    );
    let (s, v) = f.owner("/owner/device/lookup", Some(json!({"user_code":code})));
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["request"]["scope"], "workspace_operations_and_own_inbox");
    assert!(!v.to_string().contains(secret));
    assert_eq!(
        f.owner(
            "/owner/device/decision",
            Some(json!({"user_code":code,"approve":true,"days":30,"authority_confirmed":true}))
        )
        .0,
        200
    );
    let results = thread::scope(|scope| {
        let a = scope.spawn(|| {
            f.call(
                "POST",
                "/v1/device/token",
                &[],
                Some(json!({"device_code":secret})),
            )
        });
        let b = scope.spawn(|| {
            f.call(
                "POST",
                "/v1/device/token",
                &[],
                Some(json!({"device_code":secret})),
            )
        });
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(
        results.iter().filter(|r| r.0 == 200).count(),
        1,
        "{results:?}"
    );
    assert_eq!(results.iter().filter(|r| r.0 == 409).count(), 1);
    let token = results.iter().find(|r| r.0 == 200).unwrap().1["access_token"]
        .as_str()
        .unwrap();
    assert_eq!(
        f.external(token, "POST", "/v1/cli", Some(json!({"argv":["whoami"]})))
            .0,
        200
    );
    assert_eq!(f.external(token, "GET", "/v1/inbox", None).0, 200);
    assert_eq!(f.cli(&old).0, 200);
    assert_eq!(
        f.owner(
            "/owner/device/decision",
            Some(json!({"user_code":code,"approve":true,"days":30,"authority_confirmed":true}))
        )
        .0,
        401
    );
}
