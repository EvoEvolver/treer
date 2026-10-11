use super::*;
use axum::{
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, RwLock,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn serve(app: Router) -> (String, Task) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (
        url,
        Task(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        })),
    )
}

#[derive(Clone)]
struct Control {
    destination: String,
    denied: Arc<AtomicBool>,
    tokens: Arc<AtomicUsize>,
    connect_status: Arc<AtomicUsize>,
    name: Arc<RwLock<String>>,
    connect_targets: Arc<Mutex<Vec<String>>>,
}

async fn token(
    State(state): State<Control>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response<Body> {
    if body["audience"] != "svc_test"
        && body["audience"].as_str() != Some(state.name.read().unwrap().as_str())
    {
        return Response::builder().status(404).body(Body::empty()).unwrap();
    }
    assert_eq!(
        headers[WORKLOAD_CREDENTIAL_HEADER],
        format!("credential-{}", headers[AGENT_ID_HEADER].to_str().unwrap())
    );
    if state.denied.load(Ordering::SeqCst) {
        return Response::builder().status(403).body(Body::empty()).unwrap();
    }
    let sequence = state.tokens.fetch_add(1, Ordering::SeqCst);
    let token = format!(
        "token-{}-{sequence}",
        headers[AGENT_ID_HEADER].to_str().unwrap()
    );
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "access_token":token,"token_type":"Bearer","expires_in":60,
                "expires_at":"2100-01-01T00:00:00Z","audience":"svc_test"
            })
            .to_string(),
        ))
        .unwrap()
}

async fn connect(
    State(state): State<Control>,
    Path(target): Path<String>,
    mut request: Request<Body>,
) -> Response<Body> {
    state.connect_targets.lock().unwrap().push(target.clone());
    if target != "svc_test" && target != *state.name.read().unwrap() {
        return Response::builder().status(404).body(Body::empty()).unwrap();
    }
    assert_eq!(
        request.headers()[WORKLOAD_CREDENTIAL_HEADER],
        format!(
            "credential-{}",
            request.headers()[AGENT_ID_HEADER].to_str().unwrap()
        )
    );
    if state.denied.load(Ordering::SeqCst) {
        return Response::builder().status(403).body(Body::empty()).unwrap();
    }
    let status = state.connect_status.load(Ordering::SeqCst) as u16;
    if status != 101 {
        return Response::builder()
            .status(status)
            .body(Body::empty())
            .unwrap();
    }
    let upgraded = hyper::upgrade::on(&mut request);
    let mut remote = TcpStream::connect(&state.destination).await.unwrap();
    tokio::spawn(async move {
        let mut local = TokioIo::new(upgraded.await.unwrap());
        let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
    });
    Response::builder()
        .status(101)
        .header("connection", "upgrade")
        .header("upgrade", "treer-tcp")
        .body(Body::empty())
        .unwrap()
}

async fn control(destination: String) -> (WorkloadProxies, Control, Task) {
    let state = Control {
        destination,
        denied: Arc::new(AtomicBool::new(false)),
        tokens: Arc::new(AtomicUsize::new(0)),
        connect_status: Arc::new(AtomicUsize::new(101)),
        name: Arc::new(RwLock::new("paper".into())),
        connect_targets: Arc::new(Mutex::new(Vec::new())),
    };
    let router = Router::new()
        .route("/api/identity/token", post(token))
        .route("/api/services/{target}/connect", post(connect))
        .with_state(state.clone());
    let (url, task) = serve(router).await;
    (WorkloadProxies::new(url), state, task)
}

fn client(port: u16) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

#[tokio::test]
async fn http_identity_is_bound_to_entry_and_denial_never_falls_back() {
    async fn echo(request: Request<Body>) -> Json<Value> {
        let headers = request.headers();
        let identity = headers
            .get(IDENTITY_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let leaked = [
            AGENT_ID_HEADER,
            WORKLOAD_CREDENTIAL_HEADER,
            "x-treer-operator-credential",
            "proxy-authorization",
        ]
        .iter()
        .any(|name| headers.contains_key(*name));
        let body = axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        Json(
            json!({"identity":identity,"authorization":authorization,"leaked":leaked,"bytes":body.len()}),
        )
    }
    let (origin, _origin_task) = serve(Router::new().fallback(echo)).await;
    let (proxies, state, _control_task) =
        control(origin.strip_prefix("http://").unwrap().into()).await;
    let a = proxies.ensure("a", "credential-a", None).unwrap();
    let b = proxies.ensure("b", "credential-b", None).unwrap();
    assert_ne!(a, b);
    for (port, id) in [(a, "a"), (b, "b")] {
        let value: Value = client(port)
            .post("http://svc_test.treer/echo?one=two")
            .header(IDENTITY_HEADER, "spoof")
            .header(AGENT_ID_HEADER, "spoof")
            .header(WORKLOAD_CREDENTIAL_HEADER, "never-forward")
            .header("proxy-authorization", "never-forward")
            .body(vec![42; 1024 * 1024])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(value["identity"]
            .as_str()
            .unwrap()
            .starts_with(&format!("token-{id}-")));
        assert_eq!(
            value["authorization"],
            format!("Bearer {}", value["identity"].as_str().unwrap())
        );
        assert_eq!(value["leaked"], false);
        assert_eq!(value["bytes"], 1024 * 1024);
    }
    let value: Value = client(a)
        .get("http://svc_test.treer/")
        .header("authorization", "Basic native-auth")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(value["authorization"], "Basic native-auth");
    assert_eq!(state.tokens.load(Ordering::SeqCst), 3);
    let value: Value = client(a)
        .get(&origin)
        .header(IDENTITY_HEADER, "spoof")
        .header(WORKLOAD_CREDENTIAL_HEADER, "secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(value["identity"].is_null());
    assert!(value["authorization"].is_null());
    assert_eq!(value["leaked"], false);
    state.connect_status.store(403, Ordering::SeqCst);
    assert_eq!(
        client(a)
            .get("http://svc_test.treer/")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    state.connect_status.store(200, Ordering::SeqCst);
    assert_eq!(
        client(a)
            .get("http://svc_test.treer/")
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    state.connect_status.store(101, Ordering::SeqCst);
    state.denied.store(true, Ordering::SeqCst);
    assert_eq!(
        client(a)
            .get("http://svc_test.treer/")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client(a)
            .get("http://not-a-service.treer/")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn names_resolve_to_canonical_audiences_and_follow_renames_without_a_cache() {
    let (origin, _origin_task) = serve(Router::new().fallback(|headers: HeaderMap| async move {
        headers[IDENTITY_HEADER].to_str().unwrap().to_owned()
    }))
    .await;
    let (proxies, state, _control_task) =
        control(origin.strip_prefix("http://").unwrap().into()).await;
    let port = proxies.ensure("a", "credential-a", None).unwrap();
    let http = client(port);
    let response = http.get("http://PAPER.treer/").send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().starts_with("token-a-"));
    *state.name.write().unwrap() = "paper-v2".into();
    assert_eq!(
        http.get("http://paper.treer/")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        http.get("http://paper-v2.treer/")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        http.get("http://svc_test.treer/")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(*state.connect_targets.lock().unwrap(), vec!["svc_test"; 3]);
    assert_eq!(
        service_target("PaPeR.TREER.").unwrap(),
        Some("paper".into())
    );
    for host in [
        ".treer",
        "-paper.treer",
        "paper-.treer",
        "paper.test.treer",
        "paper_name.treer",
    ] {
        assert!(service_target(host).is_err(), "{host}");
    }
}

async fn tunnel(port: u16, authority: &str) -> TcpStream {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    socket
        .write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(socket.read_u8().await.unwrap());
    }
    assert!(
        header.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&header)
    );
    socket
}

#[tokio::test]
async fn connect_preserves_binary_half_close_and_entry_removal_cancels_tunnels() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = listener.local_addr().unwrap().to_string();
    let _echo = Task(tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut reader, mut writer) = socket.split();
                let _ = tokio::io::copy(&mut reader, &mut writer).await;
                let _ = writer.shutdown().await;
            });
        }
    }));
    let (proxies, state, _control_task) = control(destination.clone()).await;
    let port = proxies.ensure("a", "credential-a", None).unwrap();
    for authority in ["svc_test.treer:22", "paper.treer:22", &destination] {
        let socket = tunnel(port, authority).await;
        let (mut reader, mut writer) = socket.into_split();
        let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
        let expected = payload.clone();
        let send = async move {
            writer.write_all(&payload).await.unwrap();
            writer.shutdown().await.unwrap();
        };
        let receive = async move {
            let mut data = Vec::new();
            reader.read_to_end(&mut data).await.unwrap();
            assert_eq!(data, expected);
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(send, receive);
        })
        .await
        .unwrap();
    }
    assert_eq!(
        state.tokens.load(Ordering::SeqCst),
        0,
        "CONNECT cannot inject into opaque TLS/native protocols"
    );
    let mut active = tunnel(port, "svc_test.treer:22").await;
    proxies.remove("a");
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), active.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    tokio::task::yield_now().await;
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    assert_eq!(
        proxies.ensure("a", "credential-a", Some(port)).unwrap(),
        port
    );
    assert_eq!(proxies.ensure("a", "credential-a", None).unwrap(), port);
    assert!(proxies.ensure("a", "other-credential", None).is_err());
}
