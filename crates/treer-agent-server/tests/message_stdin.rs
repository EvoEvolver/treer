use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn message_envelope_stdin_reaches_api_without_content_in_argv_or_errors() {
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = seen.clone();
    let app = Router::new().route(
        "/api/messages",
        post(move |Json(body): Json<Value>| {
            captured.lock().unwrap().push(body);
            async { Json(json!({"accepted": true})) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let source = json!({"channel":"telegram", "conversation_id":"fixture-chat", "message_id":"42"});
    let body = "fixture private body\n私密消息";
    let envelope = serde_json::to_vec(&json!({"body":body,"external_source":source})).unwrap();
    for (flags, input, success) in [
        (vec!["--stdin-envelope"], envelope, true),
        (vec![], body.as_bytes().to_vec(), true),
        (
            vec!["--stdin-envelope"],
            br#"{"body":false,"fixture private body":1}"#.to_vec(),
            false,
        ),
    ] {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_treer-agent-server"));
        command.env_clear();
        // Only OS baseline, never inherited managed identity/proxy/harness settings.
        for name in ["SystemRoot", "WINDIR", "PATH", "TEMP", "TMP", "HOME"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .args(["--execute-cli", "message", "send", "--to", "ag_fixture"])
            .args(flags)
            .env("TREER_AGENT_SERVER_URL", &url)
            .env("TREER_WORKSPACE_ID", "ws_fixture")
            .env("TREER_AGENT_ID", "ag_sender_fixture")
            .env("TREER_WORKLOAD_CREDENTIAL", "synthetic-fixture-only")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&input).await.unwrap();
        drop(stdin);
        let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture private body"));
    }
    let requests = seen.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "invalid envelope must not send a request"
    );
    assert_eq!(requests[0]["body"], body);
    assert_eq!(requests[0]["external_source"], source);
    assert_eq!(requests[0]["recipients"], json!(["ag_fixture"]));
    assert!(requests[0].get("sender").is_none());
    assert_eq!(requests[1]["body"], body);
    assert!(requests[1]
        .get("external_source")
        .is_none_or(Value::is_null));
    server.abort();
}
