use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;
use treer_acp_launcher::{serve, AisConfig, HarnessSpec};

struct Running {
    _dir: TempDir,
    server: treer_acp_launcher::AisServer,
    base: String,
    cwd: std::path::PathBuf,
}

async fn start_fake() -> Running {
    start_fake_with(|_| {}).await
}

async fn start_fake_with(setup: impl FnOnce(&std::path::Path)) -> Running {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().join("cwd");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&cwd).unwrap();
    setup(&cwd);
    let server = serve(AisConfig {
        agent_id: "agent-1".into(),
        cwd: cwd.clone(),
        state_dir: state_dir.clone(),
        port: 0,
        ui_dist: None,
        harness: HarnessSpec::Fake,
        bind_session_id: None,
        startup_timeout_ms: 8_000,
    })
    .await
    .expect("start fake AIS");
    let base = format!("http://127.0.0.1:{}", server.port);
    Running {
        _dir: dir,
        server,
        base,
        cwd,
    }
}

async fn get_json(url: &str) -> Value {
    reqwest::get(url).await.unwrap().json().await.unwrap()
}

async fn wait_idle(base: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let status = get_json(&format!("{base}/v1/status")).await;
        if status["status"] == "idle" && status["busy"] == false {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for idle: {status}");
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn prompt(base: &str, operation_id: &str, text: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/prompts"))
        .json(&json!({ "operation_id": operation_id, "text": text }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn manifest_prompt_duplicate_and_transcript_paging() {
    let running = start_fake().await;
    let base = &running.base;

    let manifest = get_json(&format!("{base}/v1/manifest")).await;
    assert_eq!(manifest["protocol"], "treer.agent-interface/v1");
    assert_eq!(manifest["instance_id"], running.server.instance_id);
    let caps = manifest["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(caps.contains(&"prompt.submit"));
    assert!(caps.contains(&"transcript.read"));
    assert!(caps.contains(&"state.observe"));
    assert!(caps.contains(&"abort"));
    assert_eq!(manifest["ui_path"], Value::Null);

    #[cfg(not(feature = "remote-codex-ui"))]
    assert_eq!(
        reqwest::get(format!("{base}/api/state"))
            .await
            .unwrap()
            .status(),
        404
    );

    let health = get_json(&format!("{base}/v1/health")).await;
    assert_eq!(health["status"], "ok");

    let first = prompt(base, "op-1", "quick-success").await;
    assert_eq!(first.status(), 202);
    let first_body: Value = first.json().await.unwrap();
    assert_eq!(first_body["accepted"], true);
    assert!(first_body["duplicate"].is_null());

    let duplicate = prompt(base, "op-1", "quick-success").await;
    assert_eq!(duplicate.status(), 202);
    let duplicate_body: Value = duplicate.json().await.unwrap();
    assert_eq!(duplicate_body["duplicate"], true);

    wait_idle(base).await;
    let _ = prompt(base, "op-2", "second").await;
    wait_idle(base).await;

    let page0 = get_json(&format!("{base}/v1/transcript?page=0&limit=1")).await;
    assert_eq!(page0["page"], 0);
    assert_eq!(page0["page_count"], 2);
    assert_eq!(page0["next_page"], 1);
    let ids0 = page0["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids0.contains(&"op-1:user"));
    assert!(!ids0.contains(&"op-2:user"));

    let page1 = get_json(&format!("{base}/v1/transcript?page=1&limit=1")).await;
    assert_eq!(page1["next_page"], Value::Null);
    let ids1 = page1["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids1.contains(&"op-2:user"));

    running.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn abort_interrupts_a_slow_turn() {
    let running = start_fake().await;
    let base = &running.base;
    let accepted = prompt(base, "slow-1", "slow-cancel").await;
    assert_eq!(accepted.status(), 202);
    let abort = reqwest::Client::new()
        .post(format!("{base}/v1/abort"))
        .send()
        .await
        .unwrap();
    assert_eq!(abort.status(), 202);
    wait_idle(base).await;
    running.server.shutdown().await.unwrap();
}

#[tokio::test]
#[cfg(feature = "remote-codex-ui")]
async fn ui_prompt_returns_immediately_and_streams_into_state() {
    let running = start_fake().await;
    let base = &running.base;
    let started = tokio::time::Instant::now();
    let accepted = reqwest::Client::new()
        .post(format!("{base}/api/prompt"))
        .json(&json!({ "prompt": "slow-cancel" }))
        .send()
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(accepted.status().is_success());
    let surface: Value = accepted.json().await.unwrap();
    assert_eq!(surface["detail"]["thread"]["status"], "running");
    let items = surface["detail"]["turns"][0]["items"].as_array().unwrap();
    assert_eq!(items[0]["kind"], "userMessage");
    assert_eq!(items[0]["text"], "slow-cancel");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let live = get_json(&format!("{base}/api/state")).await;
        let kinds: Vec<_> = live["detail"]["turns"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["kind"].as_str())
            .collect();
        if kinds
            .iter()
            .any(|kind| *kind == "reasoning" || *kind == "toolCall" || *kind == "commandExecution")
        {
            assert_eq!(live["detail"]["thread"]["status"], "running");
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("did not stream live items before the turn finished: {kinds:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    wait_idle(base).await;
    let done = get_json(&format!("{base}/api/state")).await;
    assert_eq!(done["detail"]["thread"]["status"], "idle");
    assert_eq!(done["detail"]["turns"][0]["status"], "completed");
    running.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn permission_requests_are_auto_allowed() {
    let running = start_fake().await;
    let base = &running.base;
    assert_eq!(
        prompt(base, "perm-1", "need-permission").await.status(),
        202
    );
    wait_idle(base).await;
    let page = get_json(&format!("{base}/v1/transcript?limit=10")).await;
    let assistant = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["role"] == "assistant")
        .expect("assistant reply after auto-allow");
    assert_eq!(assistant["content"], "done");
    running.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn file_routes_are_jailed_to_cwd() {
    let running = start_fake_with(|cwd| {
        std::fs::write(cwd.join("hello.txt"), "hi").unwrap();
        std::fs::create_dir_all(cwd.join("sub")).unwrap();
        std::fs::write(cwd.join("sub/nested.txt"), "n").unwrap();
    })
    .await;
    let base = &running.base;
    let client = reqwest::Client::new();

    let tree = get_json(&format!("{base}/v1/files/tree?path=")).await;
    let names = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(names.contains(&"hello.txt"));
    assert!(names.contains(&"sub"));

    let file = get_json(&format!("{base}/v1/files?path=hello.txt")).await;
    assert_eq!(file["content"], "hi");

    let written = client
        .put(format!("{base}/v1/files?path=hello.txt"))
        .json(&json!({ "content": "updated" }))
        .send()
        .await
        .unwrap();
    assert!(written.status().is_success());
    assert_eq!(
        std::fs::read_to_string(running.cwd.join("hello.txt")).unwrap(),
        "updated"
    );

    let escaped = client
        .get(format!("{base}/v1/files/tree?path=.."))
        .send()
        .await
        .unwrap();
    assert_eq!(escaped.status(), 400);
    let escaped_write = client
        .put(format!("{base}/v1/files?path=../escape.txt"))
        .json(&json!({ "content": "no" }))
        .send()
        .await
        .unwrap();
    assert_eq!(escaped_write.status(), 400);

    running.server.shutdown().await.unwrap();
}

#[tokio::test]
#[cfg(feature = "remote-codex-ui")]
async fn ui_settings_updates_the_projected_model() {
    let running = start_fake().await;
    let base = &running.base;
    let client = reqwest::Client::new();
    let updated = client
        .post(format!("{base}/api/settings"))
        .json(&json!({ "model": "fake-2", "reasoningEffort": "high" }))
        .send()
        .await
        .unwrap();
    assert!(updated.status().is_success());
    let surface: Value = updated.json().await.unwrap();
    assert_eq!(surface["detail"]["thread"]["model"], "fake-2");
    let again = get_json(&format!("{base}/api/state")).await;
    assert_eq!(again["detail"]["thread"]["model"], "fake-2");
    running.server.shutdown().await.unwrap();
}

#[tokio::test]
#[cfg(feature = "remote-codex-ui")]
async fn serves_host_ui_dist_at_root() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().join("cwd");
    let state_dir = dir.path().join("state");
    let ui_dist = dir.path().join("ui");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&ui_dist).unwrap();
    std::fs::write(ui_dist.join("index.html"), "<html>hello-ui</html>").unwrap();
    let server = serve(AisConfig {
        agent_id: "agent-1".into(),
        cwd,
        state_dir,
        port: 0,
        ui_dist: Some(ui_dist),
        harness: HarnessSpec::Fake,
        bind_session_id: None,
        startup_timeout_ms: 8_000,
    })
    .await
    .unwrap();
    let base = format!("http://127.0.0.1:{}", server.port);
    let manifest = get_json(&format!("{base}/v1/manifest")).await;
    assert_eq!(manifest["ui_path"], "/");
    let page = reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("hello-ui"));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_survives_restart() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().join("cwd");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = || AisConfig {
        agent_id: "agent-1".into(),
        cwd: cwd.clone(),
        state_dir: state_dir.clone(),
        port: 0,
        ui_dist: None,
        harness: HarnessSpec::Fake,
        bind_session_id: None,
        startup_timeout_ms: 8_000,
    };
    let server = serve(config()).await.unwrap();
    let base = format!("http://127.0.0.1:{}", server.port);
    assert_eq!(
        prompt(&base, "op-keep", "quick-success").await.status(),
        202
    );
    wait_idle(&base).await;
    server.shutdown().await.unwrap();

    let server = serve(config()).await.unwrap();
    let base = format!("http://127.0.0.1:{}", server.port);
    let page = get_json(&format!("{base}/v1/transcript?limit=10")).await;
    let ids = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"op-keep:user"));
    server.shutdown().await.unwrap();
}
