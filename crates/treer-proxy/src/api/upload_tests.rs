//! Chunked, resumable machine upload (#166) at the router level against
//! isolated PostgreSQL and an in-process fake Controller that implements the
//! v3 commands (offset chunks with rewind, hash-verified part commits,
//! receipts, results, abort): the full 128 MiB flow in parts with bounded
//! buffering, refusals before any body or command, begin idempotency and
//! quotas, the per-upload lease, reconciliation and committing recovery,
//! expiry, and the legacy route and capability fields.
use super::tests::{test_config, test_ingress_config};
use super::*;
use axum::body::Bytes;
use sha2::Digest;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use treer_protocol::{
    CommandResult, ProxyMessage, ServerInfo, ServerStatus, CAPABILITY_MACHINE_UPLOAD,
    CAPABILITY_MACHINE_UPLOAD_V3, MACHINE_UPLOAD_CHUNK_BYTES, MACHINE_UPLOAD_MAX_BYTES,
    MACHINE_UPLOAD_PART_MAX_BYTES,
};

const MIB: u64 = 1024 * 1024;
const KEY_A: &str = "key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const KEY_B: &str = "key-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const KEY_C: &str = "key-cccccccccccccccccccccccccccccccccccc";

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}

struct FakeUpload {
    size: Option<u64>,
    data: Vec<u8>,
    committed: u64,
    parts: Vec<(u64, u64, String)>,
}

#[derive(Default)]
struct FakeState {
    uploads: HashMap<String, FakeUpload>,
    results: HashMap<String, Value>,
    begun: usize,
    chunks: usize,
    part_commits: usize,
    commits: usize,
    aborts: usize,
}

struct Fake {
    state: Arc<Mutex<FakeState>>,
    hold_chunks: Arc<AtomicBool>,
    release: Arc<tokio::sync::Notify>,
    fail_commit: Arc<Mutex<Option<&'static str>>>,
}

struct Fixture {
    auth: AuthStore,
    state: AppState,
    store: crate::upload_store::UploadStore,
    fake: Fake,
    app: Router,
    connection_a: Uuid,
}

/// A Controller-shaped ServerInfo: the Controller never fills `capabilities`.
fn server_info(id: &str, now: chrono::DateTime<chrono::Utc>) -> ServerInfo {
    let build = treer_protocol::BuildInfo {
        version: "test".into(),
        git_commit: "test".into(),
    };
    ServerInfo {
        server_id: id.into(),
        workspace_id: "default".into(),
        name: id.into(),
        hostname: id.into(),
        root: "/tmp".into(),
        controller_build: build.clone(),
        host_build: build,
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
        capabilities: vec![],
    }
}

fn answer_v3(
    fake: &Arc<Mutex<FakeState>>,
    fail_commit: &Arc<Mutex<Option<&'static str>>>,
    id: String,
    command: AgentCommand,
) -> CommandResult {
    let mut fake = fake.lock().unwrap();
    let fail = |code: &str, message: &str| {
        CommandResult::failure(id.clone(), ProtocolError::new(code, message))
    };
    match command {
        AgentCommand::UploadBegin {
            upload_id, size, ..
        } => {
            fake.begun += 1;
            fake.uploads.insert(
                upload_id,
                FakeUpload {
                    size,
                    data: Vec::new(),
                    committed: 0,
                    parts: Vec::new(),
                },
            );
            CommandResult::success(id, json!({"accepted": true}))
        }
        AgentCommand::UploadChunk {
            upload_id,
            content_base64,
            offset,
        } => {
            fake.chunks += 1;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(content_base64.as_bytes())
                .unwrap();
            assert!(decoded.len() <= MACHINE_UPLOAD_CHUNK_BYTES);
            let Some(upload) = fake.uploads.get_mut(&upload_id) else {
                return fail("machine_upload_not_found", "fixture");
            };
            let Some(offset) = offset else {
                return fail("machine_upload_mode_mismatch", "fixture");
            };
            if offset == upload.committed && upload.committed < upload.data.len() as u64 {
                upload.data.truncate(upload.committed as usize);
            } else if offset != upload.data.len() as u64 {
                return fail("machine_upload_offset", "fixture");
            }
            upload.data.extend_from_slice(&decoded);
            CommandResult::success(id, json!({"bytes_written": upload.data.len()}))
        }
        AgentCommand::UploadPartCommit {
            upload_id,
            offset,
            length,
            sha256,
        } => {
            fake.part_commits += 1;
            let Some(upload) = fake.uploads.get_mut(&upload_id) else {
                return fail("machine_upload_not_found", "fixture");
            };
            if offset + length <= upload.committed {
                return if upload.parts.contains(&(offset, length, sha256.clone())) {
                    CommandResult::success(
                        id,
                        json!({"committed_offset": upload.committed, "idempotent": true}),
                    )
                } else {
                    fail("upload_part_conflict", "fixture")
                };
            }
            if offset != upload.committed || offset + length != upload.data.len() as u64 {
                return fail("machine_upload_offset", "fixture");
            }
            if sha(&upload.data[offset as usize..]) != sha256 {
                upload.data.truncate(upload.committed as usize);
                return fail("upload_part_hash_mismatch", "fixture");
            }
            upload.committed = upload.data.len() as u64;
            upload.parts.push((offset, length, sha256));
            CommandResult::success(
                id,
                json!({"committed_offset": upload.committed, "idempotent": false}),
            )
        }
        AgentCommand::UploadCommit { upload_id } => {
            if let Some(result) = fake.results.get(&upload_id) {
                return CommandResult::success(id, result.clone());
            }
            if let Some(code) = *fail_commit.lock().unwrap() {
                fake.uploads.remove(&upload_id);
                return fail(code, "fixture");
            }
            let Some(upload) = fake.uploads.get(&upload_id) else {
                return fail("machine_upload_not_found", "fixture");
            };
            if upload
                .size
                .is_some_and(|size| size != upload.committed || size != upload.data.len() as u64)
            {
                return fail("machine_upload_size_mismatch", "fixture");
            }
            let result = json!({
                "server_id": "machine-a",
                "path": "inbox/big.bin",
                "bytes_written": upload.data.len(),
                "sha256": sha(&upload.data),
            });
            fake.uploads.remove(&upload_id);
            fake.results.insert(upload_id, result.clone());
            fake.commits += 1;
            CommandResult::success(id, result)
        }
        AgentCommand::UploadResult { upload_id } => {
            if let Some(result) = fake.results.get(&upload_id) {
                return CommandResult::success(id, json!({"state": "committed", "result": result}));
            }
            match fake.uploads.get(&upload_id) {
                Some(upload) => CommandResult::success(
                    id,
                    json!({
                        "state": "active",
                        "committed_offset": upload.committed,
                        "written_offset": upload.data.len(),
                        "parts": upload.parts.iter().map(|(o, l, s)| json!({"offset": o, "length": l, "sha256": s})).collect::<Vec<_>>(),
                    }),
                ),
                None => CommandResult::success(id, json!({"state": "unknown"})),
            }
        }
        AgentCommand::UploadAbort { upload_id } => {
            fake.aborts += 1;
            let existed = fake.uploads.remove(&upload_id).is_some();
            CommandResult::success(id, json!(existed))
        }
        _ => fail("unexpected_command", "fixture"),
    }
}

async fn fixture() -> Fixture {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let now = Utc::now();
    sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('member','member@example.test',TRUE,'Member','unused',$1),('other','other@example.test',TRUE,'Other','unused',$1)")
        .bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','member','member',$1),('org_default','other','member',$1)")
        .bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    for user in ["member", "other"] {
        sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES($1,$2,$3,'2999-01-01T00:00:00Z')")
            .bind(format!("tok-{user}")).bind(user).bind(now.to_rfc3339())
            .execute(&auth.pool()).await.unwrap();
    }
    let state = AppState::new();
    let server = |id: &str| server_info(id, now);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let connection_a = Uuid::new_v4();
    state
        .register_server_instance_with_capabilities(
            server("machine-a"),
            connection_a,
            "controller-a".into(),
            [
                CAPABILITY_MACHINE_UPLOAD.to_string(),
                CAPABILITY_MACHINE_UPLOAD_V3.to_string(),
            ],
            tx,
        )
        .await
        .unwrap();
    let (old_tx, _old_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server("machine-old"),
            Uuid::new_v4(),
            "controller-old".into(),
            [CAPABILITY_MACHINE_UPLOAD.to_string()],
            old_tx,
        )
        .await
        .unwrap();
    let fake = Fake {
        state: Arc::new(Mutex::new(FakeState::default())),
        hold_chunks: Arc::new(AtomicBool::new(false)),
        release: Arc::new(tokio::sync::Notify::new()),
        fail_commit: Arc::new(Mutex::new(None)),
    };
    {
        let (fake_state, hold, release, fail_commit, state) = (
            fake.state.clone(),
            fake.hold_chunks.clone(),
            fake.release.clone(),
            fake.fail_commit.clone(),
            state.clone(),
        );
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let SocketFrame::Text(text) = frame else {
                    continue;
                };
                let Ok(ProxyMessage::Command { envelope }) = serde_json::from_str(&text) else {
                    continue;
                };
                if matches!(envelope.command, AgentCommand::UploadChunk { .. })
                    && hold.load(Ordering::SeqCst)
                {
                    release.notified().await;
                }
                let result = answer_v3(
                    &fake_state,
                    &fail_commit,
                    envelope.command_id,
                    envelope.command,
                );
                state.complete_command(result).await;
            }
        });
    }
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let identity =
        IdentityIssuer::load(&auth, &Url::parse("https://proxy.treer.example/").unwrap())
            .await
            .unwrap();
    let app = router(
        state.clone(),
        test_config(),
        auth.clone(),
        PolicyEngine::allow_all(),
        identity,
        BrowserAccess::new(
            &Url::parse("https://app.treer.example/").unwrap(),
            &Url::parse("https://proxy.treer.example/").unwrap(),
        )
        .unwrap(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    Fixture {
        store: crate::upload_store::UploadStore::new(auth.pool()),
        auth,
        state,
        fake,
        app,
        connection_a,
    }
}

fn base(method: &str, uri: String, token: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "proxy.treer.example")
        .header(header::ORIGIN, "https://app.treer.example")
        .header(header::COOKIE, format!("treer_session={token}"))
}

fn uploads_uri(machine: &str) -> String {
    format!("/api/workspaces/default/machines/{machine}/uploads")
}

async fn json_of(response: Response) -> (StatusCode, Value, bool) {
    let status = response.status();
    let closes = response
        .headers()
        .get(header::CONNECTION)
        .is_some_and(|value| value == "close");
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    (status, value, closes)
}

fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or("")
}

async fn begin(
    fixture: &Fixture,
    token: &str,
    machine: &str,
    key: &str,
    size: u64,
    name: &str,
) -> (StatusCode, Value, bool) {
    let request = base("POST", uploads_uri(machine), token)
        .header(header::CONTENT_TYPE, "application/json")
        .header("idempotency-key", key)
        .body(Body::from(
            json!({"directory": "inbox", "file_name": name, "overwrite": false, "size": size})
                .to_string(),
        ))
        .unwrap();
    json_of(fixture.app.clone().oneshot(request).await.unwrap()).await
}

fn part_request(
    upload_id: &str,
    token: &str,
    offset: u64,
    bytes: &[u8],
    sha256: &str,
) -> Request<Body> {
    base(
        "PUT",
        format!(
            "{}/{upload_id}/parts?offset={offset}",
            uploads_uri("machine-a")
        ),
        token,
    )
    .header(header::CONTENT_TYPE, "application/octet-stream")
    .header(header::CONTENT_LENGTH, bytes.len().to_string())
    .header("x-treer-part-sha256", sha256)
    .body(Body::from(bytes.to_vec()))
    .unwrap()
}

async fn put_part(
    fixture: &Fixture,
    upload_id: &str,
    offset: u64,
    bytes: &[u8],
) -> (StatusCode, Value, bool) {
    let request = part_request(upload_id, "tok-member", offset, bytes, &sha(bytes));
    json_of(fixture.app.clone().oneshot(request).await.unwrap()).await
}

async fn simple(
    fixture: &Fixture,
    method: &str,
    path: &str,
    token: &str,
) -> (StatusCode, Value, bool) {
    let request = base(method, format!("{}{path}", uploads_uri("machine-a")), token)
        .body(Body::empty())
        .unwrap();
    json_of(fixture.app.clone().oneshot(request).await.unwrap()).await
}

fn counts(fixture: &Fixture) -> (usize, usize, usize, usize, usize) {
    let fake = fixture.fake.state.lock().unwrap();
    (
        fake.begun,
        fake.chunks,
        fake.part_commits,
        fake.commits,
        fake.aborts,
    )
}

async fn audit_payloads(auth: &AuthStore, action: &str) -> Vec<Value> {
    sqlx::query_scalar::<_, Value>(
        "SELECT payload FROM organization_audit_events WHERE action = $1 ORDER BY occurred_at",
    )
    .bind(action)
    .fetch_all(&auth.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn chunked_upload_streams_128_mib_in_parts_and_commits_once() {
    let fixture = fixture().await;
    super::agents::PART_BUFFERED_PEAK.store(0, Ordering::Relaxed);
    let total = MACHINE_UPLOAD_MAX_BYTES;
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-a", KEY_A, total, "big.bin").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["state"], "active");
    assert_eq!(body["part_bytes_max"], MACHINE_UPLOAD_PART_MAX_BYTES);
    let upload_id = body["upload_id"].as_str().unwrap().to_owned();
    assert!(
        upload_id.len() >= 36
            && upload_id.len() <= 68
            && upload_id.starts_with("upl_")
            && upload_id[4..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "upload id is upl_ + 32 to 64 lowercase hex (Web contract): {upload_id}"
    );
    let part = MACHINE_UPLOAD_PART_MAX_BYTES as usize;
    let mut whole = sha2::Sha256::new();
    let mut offset = 0_u64;
    for index in 0..(total as usize / part) {
        let bytes = vec![index as u8; part];
        whole.update(&bytes);
        let (status, body, _) = put_part(&fixture, &upload_id, offset, &bytes).await;
        assert_eq!(status, StatusCode::OK, "part {index}: {body}");
        offset += part as u64;
        assert_eq!(body["committed_offset"], offset);
    }
    let (status, body, _) = simple(
        &fixture,
        "POST",
        &format!("/{upload_id}/commit"),
        "tok-member",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "committed");
    assert_eq!(body["result"]["bytes_written"], total);
    assert_eq!(body["result"]["sha256"], format!("{:x}", whole.finalize()));
    assert!(body["result"]["committed_at"].is_string());
    let (begun, chunks, part_commits, commits, aborts) = counts(&fixture);
    assert_eq!((begun, part_commits, commits, aborts), (1, 16, 1, 0));
    assert_eq!(
        chunks as u64,
        16 * MACHINE_UPLOAD_PART_MAX_BYTES.div_ceil(MACHINE_UPLOAD_CHUNK_BYTES as u64),
        "each part is cut into its own bounded chunks"
    );
    let peak = super::agents::PART_BUFFERED_PEAK.load(Ordering::Relaxed);
    assert!(
        peak <= 3 * MACHINE_UPLOAD_CHUNK_BYTES + part,
        "buffered bytes stay bounded per part: {peak}"
    );
    // Receipt replay and status agree; the Controller is not asked again.
    let (status, replay, _) = simple(
        &fixture,
        "POST",
        &format!("/{upload_id}/commit"),
        "tok-member",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["result"], body["result"]);
    let (status, seen, _) = simple(&fixture, "GET", &format!("/{upload_id}"), "tok-member").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen["state"], "committed");
    assert_eq!(counts(&fixture).3, 1);
    let uploaded = audit_payloads(&fixture.auth, "machine.file.uploaded").await;
    assert_eq!(uploaded.len(), 1);
    assert_eq!(uploaded[0]["bytes_written"], total);
    assert_eq!(uploaded[0]["chunked"], true);
    assert_eq!(
        audit_payloads(&fixture.auth, "machine.file.upload_begun")
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn chunked_upload_refuses_before_any_body_or_command() {
    let fixture = fixture().await;
    // Begin refusals.
    let (status, body, closes) = begin(
        &fixture,
        "tok-member",
        "machine-a",
        KEY_A,
        MACHINE_UPLOAD_MAX_BYTES + 1,
        "big.bin",
    )
    .await;
    assert_eq!(
        (status, code(&body), closes),
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "machine_file_too_large",
            true
        )
    );
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-old", KEY_A, MIB, "big.bin").await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "controller_update_required")
    );
    let request = base("POST", uploads_uri("machine-a"), "tok-member")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"file_name": "a.bin", "size": 10}).to_string(),
        ))
        .unwrap();
    let (status, body, _) = json_of(fixture.app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::BAD_REQUEST, "invalid_idempotency_key")
    );
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-a", KEY_A, 10, "../escape").await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::BAD_REQUEST, "invalid_machine_file_name")
    );
    assert_eq!(
        counts(&fixture).0,
        0,
        "no Controller command for a refused begin"
    );
    // A real upload of 3 MiB.
    let (status, body, _) = begin(
        &fixture,
        "tok-member",
        "machine-a",
        KEY_A,
        3 * MIB,
        "three.bin",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let upload_id = body["upload_id"].as_str().unwrap().to_owned();
    let first = vec![1_u8; MIB as usize];
    // Another member never sees it.
    for (method, path) in [("GET", ""), ("POST", "/commit"), ("DELETE", "")] {
        let (status, body, _) = simple(
            &fixture,
            method,
            &format!("/{upload_id}{path}"),
            "tok-other",
        )
        .await;
        assert_eq!(
            (status, code(&body)),
            (StatusCode::NOT_FOUND, "upload_not_found"),
            "{method}"
        );
    }
    let foreign = part_request(&upload_id, "tok-other", 0, &first, &sha(&first));
    let (status, body, _) = json_of(fixture.app.clone().oneshot(foreign).await.unwrap()).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::NOT_FOUND, "upload_not_found")
    );
    // Part shape refusals before the body is read.
    let small = vec![0_u8; 512 * 1024];
    let (status, body, closes) = put_part(&fixture, &upload_id, 0, &small).await;
    assert_eq!(
        (status, code(&body), closes),
        (StatusCode::BAD_REQUEST, "upload_part_too_small", true)
    );
    let (status, body, _) =
        put_part(&fixture, &upload_id, 2 * MIB, &vec![0_u8; 2 * MIB as usize]).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::PAYLOAD_TOO_LARGE, "upload_overrun")
    );
    let (status, body, _) = put_part(&fixture, &upload_id, MIB, &first).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "upload_offset_gap")
    );
    assert_eq!(body["error"]["hint"][0], "committed_offset=0");
    let chunked = base(
        "PUT",
        format!("{}/{upload_id}/parts?offset=0", uploads_uri("machine-a")),
        "tok-member",
    )
    .header(header::TRANSFER_ENCODING, "chunked")
    .header("x-treer-part-sha256", sha(&first))
    .body(Body::from(first.clone()))
    .unwrap();
    let (status, body, closes) = json_of(fixture.app.clone().oneshot(chunked).await.unwrap()).await;
    assert_eq!(
        (status, code(&body), closes),
        (StatusCode::LENGTH_REQUIRED, "length_required", true)
    );
    assert_eq!(counts(&fixture).1, 0, "no chunk was forwarded");
    // A wrong hash header discards the part on both sides; the retry lands.
    let wrong = part_request(&upload_id, "tok-member", 0, &first, &sha(b"other"));
    let (status, body, _) = json_of(fixture.app.clone().oneshot(wrong).await.unwrap()).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::BAD_REQUEST, "upload_part_hash_mismatch")
    );
    let (status, seen, _) = simple(&fixture, "GET", &format!("/{upload_id}"), "tok-member").await;
    assert_eq!(
        (status, seen["committed_offset"].as_u64()),
        (StatusCode::OK, Some(0))
    );
    let (status, body, _) = put_part(&fixture, &upload_id, 0, &first).await;
    assert_eq!(
        (status, body["committed_offset"].as_u64()),
        (StatusCode::OK, Some(MIB))
    );
    let fake_data_len = fixture.fake.state.lock().unwrap().uploads[&upload_id]
        .data
        .len();
    assert_eq!(
        fake_data_len as u64, MIB,
        "the Controller rewound the bad part"
    );
    // Replays of the committed part: identical is idempotent without any
    // command, a different hash conflicts.
    let before = counts(&fixture);
    let (status, body, _) = put_part(&fixture, &upload_id, 0, &first).await;
    assert_eq!(
        (status, body["idempotent"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    assert_eq!(counts(&fixture), before);
    let conflict = part_request(&upload_id, "tok-member", 0, &first, &sha(b"different"));
    let (status, body, _) = json_of(fixture.app.clone().oneshot(conflict).await.unwrap()).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "upload_part_conflict")
    );
    // Commit before completion is refused with the offset.
    let (status, body, _) = simple(
        &fixture,
        "POST",
        &format!("/{upload_id}/commit"),
        "tok-member",
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "upload_incomplete")
    );
    assert_eq!(
        body["error"]["hint"][0],
        format!("committed_offset={MIB} size={}", 3 * MIB)
    );
}

#[tokio::test]
async fn chunked_upload_begin_is_idempotent_and_quotas_hold_in_the_database() {
    let fixture = fixture().await;
    let (status, first, _) = begin(&fixture, "tok-member", "machine-a", KEY_A, MIB, "a.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, replay, _) = begin(&fixture, "tok-member", "machine-a", KEY_A, MIB, "a.bin").await;
    assert_eq!(status, StatusCode::OK, "same key and parameters replays");
    assert_eq!(replay["upload_id"], first["upload_id"]);
    assert_eq!(counts(&fixture).0, 1, "no second Controller begin");
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-a", KEY_A, 2 * MIB, "a.bin").await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "upload_begin_conflict")
    );
    // Second upload fits the per-user quota, the third does not; another
    // member is unaffected; aborting frees the slot.
    let (status, second, _) = begin(&fixture, "tok-member", "machine-a", KEY_B, MIB, "b.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body, closes) =
        begin(&fixture, "tok-member", "machine-a", KEY_C, MIB, "c.bin").await;
    assert_eq!(
        (status, code(&body), closes),
        (StatusCode::TOO_MANY_REQUESTS, "machine_upload_busy", true)
    );
    let (status, _, _) = begin(&fixture, "tok-other", "machine-a", KEY_A, MIB, "o.bin").await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "another member has its own slots"
    );
    let (status, listed, _) = simple(&fixture, "GET", "", "tok-member").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["uploads"].as_array().unwrap().len(), 2);
    let (status, listed, _) = simple(
        &fixture,
        "GET",
        &format!("?idempotency_key={KEY_B}"),
        "tok-member",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["uploads"][0]["upload_id"], second["upload_id"]);
    let (status, aborted, _) = simple(
        &fixture,
        "DELETE",
        &format!("/{}", second["upload_id"].as_str().unwrap()),
        "tok-member",
    )
    .await;
    assert_eq!(
        (status, aborted["state"].as_str()),
        (StatusCode::OK, Some("aborted"))
    );
    assert_eq!(aborted["failure_code"], "client_abort");
    let (status, _, _) = begin(&fixture, "tok-member", "machine-a", KEY_C, MIB, "c.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(counts(&fixture).4, 1);
}

#[tokio::test]
async fn chunked_upload_lease_serializes_parts_and_protects_abort() {
    let fixture = fixture().await;
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-a", KEY_A, 2 * MIB, "l.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    let upload_id = body["upload_id"].as_str().unwrap().to_owned();
    fixture.fake.hold_chunks.store(true, Ordering::SeqCst);
    let first = vec![7_u8; MIB as usize];
    let held = {
        let app = fixture.app.clone();
        let request = part_request(&upload_id, "tok-member", 0, &first, &sha(&first));
        tokio::spawn(async move { json_of(app.oneshot(request).await.unwrap()).await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.state.upload_gate().in_flight() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first part admitted");
    let second = vec![8_u8; MIB as usize];
    // A concurrent part at the same committed offset is refused by the lease.
    let busy = part_request(&upload_id, "tok-member", 0, &second, &sha(&second));
    let response = fixture.app.clone().oneshot(busy).await.unwrap();
    assert_eq!(response.headers()[header::RETRY_AFTER], "5");
    let (status, body, closes) = json_of(response).await;
    assert_eq!(
        (status, code(&body), closes),
        (StatusCode::CONFLICT, "upload_busy", true)
    );
    let (status, body, _) =
        simple(&fixture, "DELETE", &format!("/{upload_id}"), "tok-member").await;
    assert_eq!((status, code(&body)), (StatusCode::CONFLICT, "upload_busy"));
    fixture.fake.hold_chunks.store(false, Ordering::SeqCst);
    fixture.fake.release.notify_waiters();
    let (status, body, _) = held.await.unwrap();
    assert_eq!(
        (status, body["committed_offset"].as_u64()),
        (StatusCode::OK, Some(MIB))
    );
    let (status, body, _) = put_part(&fixture, &upload_id, MIB, &second).await;
    assert_eq!(
        (status, body["committed_offset"].as_u64()),
        (StatusCode::OK, Some(2 * MIB))
    );
    let (status, body, _) =
        simple(&fixture, "DELETE", &format!("/{upload_id}"), "tok-member").await;
    assert_eq!(
        (status, body["state"].as_str()),
        (StatusCode::OK, Some("aborted"))
    );
    assert_eq!(counts(&fixture).4, 1);
    // A committed upload cannot be aborted into a lie.
    let (status, body, _) = begin(&fixture, "tok-member", "machine-a", KEY_B, MIB, "m.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    let upload_id = body["upload_id"].as_str().unwrap().to_owned();
    put_part(&fixture, &upload_id, 0, &first).await;
    let (status, body, _) = simple(
        &fixture,
        "POST",
        &format!("/{upload_id}/commit"),
        "tok-member",
    )
    .await;
    assert_eq!(
        (status, body["state"].as_str()),
        (StatusCode::OK, Some("committed"))
    );
    let (status, body, _) =
        simple(&fixture, "DELETE", &format!("/{upload_id}"), "tok-member").await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "upload_committed"),
        "a published upload is never reported aborted"
    );
    let aborted = audit_payloads(&fixture.auth, "machine.file.upload_aborted").await;
    assert_eq!(aborted.len(), 1);
    assert_eq!(aborted[0]["reason"], "client_abort");
}

#[tokio::test]
async fn chunked_upload_status_reconciles_and_expiry_recovers_or_expires() {
    let fixture = fixture().await;
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-a", KEY_A, 2 * MIB, "r.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    let upload_id = body["upload_id"].as_str().unwrap().to_owned();
    let first = vec![3_u8; MIB as usize];
    let (status, _, _) = put_part(&fixture, &upload_id, 0, &first).await;
    assert_eq!(status, StatusCode::OK);
    // The row lost the part response: the Controller is ahead and status
    // adopts its offset and parts.
    sqlx::query(
        "UPDATE machine_uploads SET committed_offset = 0, parts = '[]'::jsonb WHERE upload_id = $1",
    )
    .bind(&upload_id)
    .execute(&fixture.auth.pool())
    .await
    .unwrap();
    let (status, seen, _) = simple(&fixture, "GET", &format!("/{upload_id}"), "tok-member").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen["committed_offset"], MIB);
    assert_eq!(seen["reconciled"], true);
    let second = vec![4_u8; MIB as usize];
    let (status, body, _) = put_part(&fixture, &upload_id, MIB, &second).await;
    assert_eq!(
        (status, body["committed_offset"].as_u64()),
        (StatusCode::OK, Some(2 * MIB))
    );
    let (status, body, _) = simple(
        &fixture,
        "POST",
        &format!("/{upload_id}/commit"),
        "tok-member",
    )
    .await;
    assert_eq!(
        (status, body["state"].as_str()),
        (StatusCode::OK, Some("committed"))
    );
    // A Proxy that died between the Controller's publish and the row update
    // left `committing`; the expiry job recovers the receipt.
    sqlx::query("UPDATE machine_uploads SET state = 'committing', result = NULL, finished_at = NULL, lease_until = '2000-01-01T00:00:00.000000Z' WHERE upload_id = $1")
        .bind(&upload_id)
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    super::agents::expire_machine_uploads(&fixture.state, &fixture.auth, &fixture.store).await;
    let (status, seen, _) = simple(&fixture, "GET", &format!("/{upload_id}"), "tok-member").await;
    assert_eq!(
        (status, seen["state"].as_str()),
        (StatusCode::OK, Some("committed"))
    );
    assert_eq!(seen["result"]["bytes_written"], 2 * MIB);
    let uploaded = audit_payloads(&fixture.auth, "machine.file.uploaded").await;
    assert_eq!(uploaded.len(), 2);
    assert_eq!(uploaded[1]["recovered"], true);
    // Idle expiry aborts on the Controller and marks the row; retention
    // purges terminal rows.
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-a", KEY_B, MIB, "idle.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    let idle_id = body["upload_id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE machine_uploads SET last_part_at = '2000-01-01T00:00:00.000000Z' WHERE upload_id = $1")
        .bind(&idle_id)
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    let aborts_before = counts(&fixture).4;
    super::agents::expire_machine_uploads(&fixture.state, &fixture.auth, &fixture.store).await;
    assert_eq!(counts(&fixture).4, aborts_before + 1);
    let (status, seen, _) = simple(&fixture, "GET", &format!("/{idle_id}"), "tok-member").await;
    assert_eq!(
        (
            status,
            seen["state"].as_str(),
            seen["failure_code"].as_str()
        ),
        (StatusCode::OK, Some("expired"), Some("idle_expired"))
    );
    sqlx::query("UPDATE machine_uploads SET finished_at = '2000-01-01T00:00:00.000000Z' WHERE upload_id = $1")
        .bind(&idle_id)
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    super::agents::expire_machine_uploads(&fixture.state, &fixture.auth, &fixture.store).await;
    let (status, body, _) = simple(&fixture, "GET", &format!("/{idle_id}"), "tok-member").await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::NOT_FOUND, "upload_not_found")
    );
    // A Controller that forgot an active upload (restart) ends it as
    // upload_lost; a committing row with a generic Controller error or no
    // evidence stays committing and only the lifetime ends it as
    // outcome_unknown.
    let (status, body, _) = begin(
        &fixture,
        "tok-member",
        "machine-a",
        "key-dddddddddddddddddddddddddddddddddddd",
        MIB,
        "lost.bin",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let lost_id = body["upload_id"].as_str().unwrap().to_owned();
    fixture.fake.state.lock().unwrap().uploads.remove(&lost_id);
    let (status, seen, _) = simple(&fixture, "GET", &format!("/{lost_id}"), "tok-member").await;
    assert_eq!(
        (
            status,
            seen["state"].as_str(),
            seen["failure_code"].as_str()
        ),
        (StatusCode::OK, Some("failed"), Some("upload_lost"))
    );
    let (status, body, _) = begin(
        &fixture,
        "tok-member",
        "machine-a",
        "key-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        MIB,
        "generic.bin",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let generic_id = body["upload_id"].as_str().unwrap().to_owned();
    put_part(&fixture, &generic_id, 0, &first).await;
    *fixture.fake.fail_commit.lock().unwrap() = Some("state_error");
    let (status, body, _) = simple(
        &fixture,
        "POST",
        &format!("/{generic_id}/commit"),
        "tok-member",
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::SERVICE_UNAVAILABLE, "controller_unavailable")
    );
    assert_eq!(body["state"], "committing");
    assert_eq!(body["reconciled"], false);
    *fixture.fake.fail_commit.lock().unwrap() = None;
    // The fake dropped the upload on that failure, so the Controller now
    // answers unknown: still committing, until the lifetime ends it.
    super::agents::expire_machine_uploads(&fixture.state, &fixture.auth, &fixture.store).await;
    let (_, seen, _) = simple(&fixture, "GET", &format!("/{generic_id}"), "tok-member").await;
    assert_eq!(
        (seen["state"].as_str(), seen["reconciled"].as_bool()),
        (Some("committing"), Some(true))
    );
    sqlx::query("UPDATE machine_uploads SET created_at = '2000-01-01T00:00:00.000000Z' WHERE upload_id = $1")
        .bind(&generic_id)
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    super::agents::expire_machine_uploads(&fixture.state, &fixture.auth, &fixture.store).await;
    let (_, seen, _) = simple(&fixture, "GET", &format!("/{generic_id}"), "tok-member").await;
    assert_eq!(
        (
            seen["state"].as_str(),
            seen["failure_code"].as_str(),
            seen["terminal"].as_bool()
        ),
        (Some("failed"), Some("outcome_unknown"), Some(true))
    );
    // A failed publish is a terminal, fixed-code state.
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-a", KEY_C, MIB, "taken.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    let taken_id = body["upload_id"].as_str().unwrap().to_owned();
    put_part(&fixture, &taken_id, 0, &first).await;
    *fixture.fake.fail_commit.lock().unwrap() = Some("machine_file_exists");
    let (status, body, _) = simple(
        &fixture,
        "POST",
        &format!("/{taken_id}/commit"),
        "tok-member",
    )
    .await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::CONFLICT, "machine_file_exists")
    );
    let (status, seen, _) = simple(&fixture, "GET", &format!("/{taken_id}"), "tok-member").await;
    assert_eq!(
        (
            status,
            seen["state"].as_str(),
            seen["failure_code"].as_str()
        ),
        (StatusCode::OK, Some("failed"), Some("machine_file_exists"))
    );
}

#[tokio::test]
async fn chunked_upload_part_timing_releases_the_lease_and_legacy_route_is_unchanged() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            super::agents::set_part_windows_for_test(
                Duration::from_secs(30),
                Duration::from_secs(120),
            );
        }
    }
    let _restore = Restore;
    super::agents::set_part_windows_for_test(Duration::from_millis(300), Duration::from_secs(10));
    let fixture = fixture().await;
    let (status, body, _) =
        begin(&fixture, "tok-member", "machine-a", KEY_A, MIB, "slow.bin").await;
    assert_eq!(status, StatusCode::CREATED);
    let upload_id = body["upload_id"].as_str().unwrap().to_owned();
    let first = vec![9_u8; MIB as usize];
    let idle = Body::from_stream(futures_util::stream::once(async {
        tokio::time::sleep(Duration::from_millis(600)).await;
        Ok::<_, std::io::Error>(Bytes::from(vec![9_u8; MIB as usize]))
    }));
    let request = base(
        "PUT",
        format!("{}/{upload_id}/parts?offset=0", uploads_uri("machine-a")),
        "tok-member",
    )
    .header(header::CONTENT_LENGTH, MIB.to_string())
    .header("x-treer-part-sha256", sha(&first))
    .body(idle)
    .unwrap();
    let (status, body, closes) = json_of(fixture.app.clone().oneshot(request).await.unwrap()).await;
    assert_eq!(
        (status, code(&body), closes),
        (StatusCode::REQUEST_TIMEOUT, "machine_upload_idle", true)
    );
    // The lease is released, so the retry is admitted and lands.
    let (status, body, _) = put_part(&fixture, &upload_id, 0, &first).await;
    assert_eq!(
        (status, body["committed_offset"].as_u64()),
        (StatusCode::OK, Some(MIB)),
        "{body}"
    );
    // Legacy route keeps its 23 MiB JSON body limit; capabilities are listed.
    let response = fixture
        .app
        .clone()
        .oneshot(
            base(
                "POST",
                "/api/workspaces/default/machines/machine-a/files".into(),
                "tok-member",
            )
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, (24 * MIB).to_string())
            .body(Body::from(vec![b'{'; 24 * MIB as usize]))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let Json(health) = super::health().await;
    assert!(health["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!(CAPABILITY_MACHINE_UPLOAD_V3)));
    let response = fixture
        .app
        .clone()
        .oneshot(
            base(
                "GET",
                "/api/workspaces/default/snapshot".into(),
                "tok-member",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let (status, snapshot, _) = json_of(response).await;
    assert_eq!(status, StatusCode::OK);
    let servers = snapshot["servers"].as_array().unwrap();
    let a = servers
        .iter()
        .find(|s| s["server_id"] == "machine-a")
        .unwrap();
    let old = servers
        .iter()
        .find(|s| s["server_id"] == "machine-old")
        .unwrap();
    assert!(a["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!(CAPABILITY_MACHINE_UPLOAD_V3)));
    assert!(!old["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!(CAPABILITY_MACHINE_UPLOAD_V3)));
}

#[tokio::test]
async fn read_models_keep_live_capabilities_after_a_controller_snapshot() {
    let fixture = fixture().await;
    // A Controller snapshot carries a ServerInfo without capabilities; the
    // Proxy must keep the capabilities it learned at registration (#166).
    fixture
        .state
        .apply_snapshot(
            fixture.connection_a,
            treer_protocol::AgentServerSnapshot {
                server: server_info("machine-a", Utc::now()),
                agents: vec![],
            },
        )
        .await
        .unwrap();
    for uri in [
        "/api/workspaces/default/snapshot",
        "/api/workspaces/default/servers",
    ] {
        let request = base("GET", uri.to_string(), "tok-member")
            .body(Body::empty())
            .unwrap();
        let (status, body, _) = json_of(fixture.app.clone().oneshot(request).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let servers = body
            .as_array()
            .cloned()
            .or_else(|| body["servers"].as_array().cloned())
            .unwrap_or_else(|| panic!("{uri}: no servers array in {body}"));
        let capabilities = |id: &str| -> Vec<String> {
            servers
                .iter()
                .find(|server| server["server_id"] == id)
                .unwrap_or_else(|| panic!("{uri}: {id} missing in {body}"))["capabilities"]
                .as_array()
                .unwrap_or_else(|| panic!("{uri}: {id} has no capabilities array"))
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            capabilities("machine-a"),
            vec![
                CAPABILITY_MACHINE_UPLOAD.to_string(),
                CAPABILITY_MACHINE_UPLOAD_V3.to_string()
            ],
            "{uri}"
        );
        assert_eq!(
            capabilities("machine-old"),
            vec![CAPABILITY_MACHINE_UPLOAD.to_string()],
            "{uri}"
        );
    }
}
