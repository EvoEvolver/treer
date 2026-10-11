use std::sync::{Arc, Mutex};

use axum::{extract::State, http::HeaderMap, routing::get, Json, Router};
use serde_json::{json, Value};
use tokio::process::Command;

#[derive(Default)]
struct Registry {
    services: Vec<Value>,
    mutations: Vec<(String, Value)>,
    deny_update: bool,
}

type Shared = Arc<Mutex<Registry>>;

fn authenticate(headers: &HeaderMap) {
    assert_eq!(headers["x-treer-agent-id"], "agent-reviewer");
    assert_eq!(headers["x-treer-workload-credential"], "test-credential");
}

async fn agent(headers: HeaderMap) -> Json<Value> {
    authenticate(&headers);
    Json(json!({
        "agent_id": "agent-reviewer", "workspace_id": "workspace-a",
        "server_id": "machine-a", "kind": "command", "name": "reviewer",
        "cwd": ".", "status": "working", "started_at": "2026-10-11T00:00:00Z",
        "updated_at": "2026-10-11T00:00:00Z"
    }))
}

async fn services(State(state): State<Shared>, headers: HeaderMap) -> Json<Value> {
    authenticate(&headers);
    Json(json!({"services": state.lock().unwrap().services}))
}

fn service(id: &str, agent: &str, primary: bool) -> Value {
    json!({
        "service_id": id, "workspace_id": "workspace-a", "name": id,
        "server_id": "machine-a", "target_agent_id": agent, "primary": primary,
        "target_host": "127.0.0.1", "target_port": 4180, "protocol": "http",
        "created_at": "2026-10-11T00:00:00Z", "created_by": agent,
        "updated_at": "2026-10-11T00:00:00Z", "updated_by": agent
    })
}

async fn register(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    authenticate(&headers);
    let mut registry = state.lock().unwrap();
    registry.mutations.push(("POST".into(), body.clone()));
    let mut endpoint = service(
        "svc_stable",
        "agent-reviewer",
        body["primary"].as_bool().unwrap(),
    );
    endpoint["name"] = body["name"].clone();
    endpoint["target_port"] = body["target_port"].clone();
    registry.services.push(endpoint.clone());
    Json(json!({"service": endpoint}))
}

async fn update(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    authenticate(&headers);
    let mut registry = state.lock().unwrap();
    if registry.deny_update {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({"error": {"code": "policy_denied", "message": "update denied"}})),
        ));
    }
    registry.mutations.push(("PATCH".into(), body.clone()));
    let endpoint = registry
        .services
        .iter_mut()
        .find(|entry| entry["service_id"] == "svc_stable")
        .unwrap();
    endpoint["name"] = body["name"].clone();
    endpoint["target_port"] = body["target_port"].clone();
    endpoint["target_host"] = body["target_host"].clone();
    endpoint["primary"] = body["primary"].clone();
    endpoint["protocol"] = body["protocol"].clone();
    Ok(Json(json!({"service": endpoint})))
}

async fn fixture(state: Shared) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/api/agents/{target}", get(agent))
        .route("/api/services", get(services).post(register))
        .route("/api/services/{service_id}", axum::routing::patch(update))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, task)
}

async fn cli(base: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_treer-agent-server"))
        .arg("--execute-cli")
        .args(args)
        .env("TREER_AGENT_SERVER_URL", base)
        .env("TREER_WORKSPACE_ID", "workspace-a")
        .env("TREER_AGENT_ID", "agent-reviewer")
        .env("TREER_SERVER_ID", "machine-a")
        .env("TREER_WORKLOAD_CREDENTIAL", "test-credential")
        .env_remove("TREER_OPERATOR_CREDENTIAL")
        .output()
        .await
        .unwrap()
}

#[tokio::test]
async fn address_is_shell_ready_and_primary_registration_survives_port_and_name_changes() {
    let state = Shared::default();
    // Multiple services are allowed; only this Agent's explicit primary counts.
    state.lock().unwrap().services = vec![
        service("svc_other_agent", "agent-other", true),
        service("svc_secondary", "agent-reviewer", false),
    ];
    let (base, task) = fixture(state.clone()).await;
    let missing = cli(&base, &["agent", "address", "reviewer"]).await;
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8(missing.stderr)
        .unwrap()
        .contains("no primary HTTP service"));

    let registration = cli(
        &base,
        &[
            "service",
            "register",
            "--primary",
            "--protocol",
            "http",
            "--name",
            "review-api",
            "--port",
            "4180",
        ],
    )
    .await;
    assert!(registration.status.success(), "{:?}", registration);
    let before = cli(&base, &["agent", "address", "reviewer"]).await;
    assert!(before.status.success(), "{:?}", before);
    assert_eq!(before.stdout, b"http://svc_stable.treer/\n");
    assert!(before.stderr.is_empty());

    let registration = cli(
        &base,
        &[
            "service",
            "register",
            "--primary",
            "--protocol",
            "http",
            "--name",
            "renamed-api",
            "--port",
            "4199",
        ],
    )
    .await;
    assert!(registration.status.success(), "{:?}", registration);
    let after = cli(&base, &["agent", "address"]).await;
    assert!(after.status.success());
    assert_eq!(before.stdout, after.stdout);
    let metadata = cli(&base, &["agent", "address", "agent-reviewer", "--json"]).await;
    assert!(metadata.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&metadata.stdout).unwrap(),
        json!({
            "agent_id": "agent-reviewer", "service_id": "svc_stable",
            "address": "http://svc_stable.treer/"
        })
    );
    {
        let registry = state.lock().unwrap();
        assert_eq!(registry.services.len(), 3);
        assert_eq!(registry.mutations[0].0, "POST");
        assert_eq!(registry.mutations[0].1["primary"], true);
        assert_eq!(registry.mutations[1].0, "PATCH");
        assert_eq!(registry.mutations[1].1["target_port"], 4199);
        assert_eq!(registry.mutations[1].1["name"], "renamed-api");
    }
    // Re-registration must not bypass the existing service.update policy.
    state.lock().unwrap().deny_update = true;
    let denied = cli(
        &base,
        &[
            "service",
            "register",
            "--primary",
            "--protocol",
            "http",
            "--name",
            "review-api",
            "--port",
            "4200",
        ],
    )
    .await;
    assert!(!denied.status.success());
    assert!(String::from_utf8(denied.stderr)
        .unwrap()
        .contains("policy_denied"));
    assert_eq!(state.lock().unwrap().mutations.len(), 2);
    task.abort();
}

#[tokio::test]
async fn primary_registration_promotes_an_existing_owned_service_without_changing_its_id() {
    let state = Shared::default();
    let mut existing = service("svc_stable", "agent-reviewer", false);
    existing["name"] = json!("review-api");
    state.lock().unwrap().services.push(existing);
    let (base, task) = fixture(state.clone()).await;
    let output = cli(
        &base,
        &[
            "service",
            "register",
            "--primary",
            "--protocol",
            "http",
            "--name",
            "review-api",
            "--port",
            "4180",
        ],
    )
    .await;
    assert!(output.status.success(), "{:?}", output);
    let address = cli(&base, &["agent", "address", "reviewer"]).await;
    assert!(address.status.success());
    assert_eq!(address.stdout, b"http://svc_stable.treer/\n");
    assert_eq!(state.lock().unwrap().mutations[0].0, "PATCH");
    assert_eq!(state.lock().unwrap().services.len(), 1);
    task.abort();
}

#[tokio::test]
async fn primary_tcp_is_rejected_without_mutation() {
    let state = Shared::default();
    let (base, task) = fixture(state.clone()).await;
    let output = cli(
        &base,
        &[
            "service",
            "register",
            "--primary",
            "--name",
            "review-api",
            "--port",
            "4180",
        ],
    )
    .await;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("--protocol http"));
    assert!(state.lock().unwrap().mutations.is_empty());
    task.abort();
}
