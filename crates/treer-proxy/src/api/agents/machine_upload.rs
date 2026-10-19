//! Chunked, resumable machine upload (#166): begin, part, status, commit,
//! abort and list. Every request re-authenticates, requires the exact Console
//! origin (Console-class route), re-evaluates `machine.file.write`, and must
//! come from the subject that began the upload (anyone else sees 404). Rows
//! live in `upload_store`; bytes live in the Controller's staging file; one
//! part is in flight per upload through a database lease; a part body is
//! streamed in 192 KiB chunks with two commands in flight, hashed here and
//! verified again by the Controller before the committed offset advances.
use super::*;
use crate::upload_gate::{
    BodyPermit, PARTS_PER_UPLOAD_PER_MINUTE, RETRY_AFTER_SECONDS, STATUS_PER_UPLOAD_PER_MINUTE,
};
use crate::upload_store::{
    BeginOutcome, BeginParams, PartRecord, UploadReceipt, UploadRow, UploadState, UploadStore,
};
use chrono::Utc;
use futures_util::StreamExt;
use sha2::Digest;
use std::collections::VecDeque;
use treer_protocol::{
    MACHINE_UPLOAD_CHUNK_BYTES, MACHINE_UPLOAD_IDLE_SECONDS, MACHINE_UPLOAD_MAX_BYTES,
    MACHINE_UPLOAD_MAX_LIFETIME_SECONDS, MACHINE_UPLOAD_MAX_PARTS, MACHINE_UPLOAD_PART_MAX_BYTES,
    MACHINE_UPLOAD_PART_MIN_BYTES,
};

const CHUNKS_IN_FLIGHT: usize = 2;
/// A part admitted for streaming: the leased row, the declared length, the
/// expected hash, the instance body slot and the lease guard.
type AdmittedPart = (UploadRow, u64, String, BodyPermit, LeaseGuard);
const PART_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const PART_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
const PART_SHA256_HEADER: &str = "x-treer-part-sha256";

static PART_IDLE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(PART_IDLE_TIMEOUT.as_millis() as u64);
static PART_TOTAL_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(PART_TOTAL_TIMEOUT.as_millis() as u64);

#[cfg(test)]
pub(crate) static PART_BUFFERED_PEAK: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn set_part_windows_for_test(idle: Duration, total: Duration) {
    use std::sync::atomic::Ordering::Relaxed;
    PART_IDLE_MS.store(idle.as_millis() as u64, Relaxed);
    PART_TOTAL_MS.store(total.as_millis() as u64, Relaxed);
}

fn window(cell: &std::sync::atomic::AtomicU64) -> Duration {
    Duration::from_millis(cell.load(std::sync::atomic::Ordering::Relaxed))
}

#[derive(Deserialize)]
pub(crate) struct MachineUploadBeginRequest {
    #[serde(default)]
    directory: String,
    file_name: String,
    #[serde(default)]
    overwrite: bool,
    size: u64,
}

#[derive(Deserialize)]
pub(crate) struct UploadPartQuery {
    offset: u64,
}

#[derive(Deserialize)]
pub(crate) struct UploadListQuery {
    #[serde(default)]
    idempotency_key: Option<String>,
}

/// Every refusal from these routes closes the connection: a part client may
/// still be sending a body we never read.
fn closing(failure: ApiFailure) -> Response {
    let mut response = failure.into_response();
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    if matches!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS | StatusCode::CONFLICT
    ) {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&RETRY_AFTER_SECONDS.to_string()).unwrap(),
        );
    }
    response
}

fn conflict(code: &str, message: &str) -> ApiFailure {
    ApiFailure {
        status: StatusCode::CONFLICT,
        error: ProtocolError::new(code, message),
    }
}

fn too_large(code: &str, message: &str) -> ApiFailure {
    ApiFailure {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        error: ProtocolError::new(code, message),
    }
}

fn too_many(code: &str, message: &str) -> ApiFailure {
    ApiFailure {
        status: StatusCode::TOO_MANY_REQUESTS,
        error: ProtocolError::new(code, message),
    }
}

fn store_error(error: crate::upload_store::UploadStoreError) -> ApiFailure {
    tracing::warn!(error = ?error.0, "machine upload store failure");
    ApiFailure::internal("upload_store_error", "the upload registry is unavailable")
}

fn not_found() -> ApiFailure {
    ApiFailure::not_found("upload_not_found", "upload does not exist")
}

fn validate_begin(request: &MachineUploadBeginRequest) -> Result<(), ApiFailure> {
    let name = &request.file_name;
    if name.is_empty()
        || name.len() > 255
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
        || name == "."
        || name == ".."
    {
        return Err(ApiFailure::bad_request(
            "invalid_machine_file_name",
            "file_name must be one file name without path separators",
        ));
    }
    if request.directory.len() > 4096
        || request.directory.chars().any(char::is_control)
        || std::path::Path::new(&request.directory).is_absolute()
    {
        return Err(ApiFailure::bad_request(
            "invalid_machine_path",
            "directory must be a relative path beneath the machine root",
        ));
    }
    if request.size > MACHINE_UPLOAD_MAX_BYTES {
        return Err(too_large(
            "machine_file_too_large",
            &format!("uploaded files may contain at most {MACHINE_UPLOAD_MAX_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, ApiFailure> {
    let mut values = headers.get_all("idempotency-key").iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(ApiFailure::bad_request(
            "invalid_idempotency_key",
            "begin requires exactly one Idempotency-Key header of 32 to 128 characters",
        ));
    };
    let key = value.to_str().unwrap_or_default();
    if key.len() < 32
        || key.len() > 128
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ApiFailure::bad_request(
            "invalid_idempotency_key",
            "Idempotency-Key must be 32 to 128 characters of letters, digits, _ or -",
        ));
    }
    Ok(key.to_owned())
}

fn declared_length(headers: &HeaderMap) -> Result<u64, ApiFailure> {
    if headers.contains_key(header::TRANSFER_ENCODING) {
        return Err(ApiFailure {
            status: StatusCode::LENGTH_REQUIRED,
            error: ProtocolError::new(
                "length_required",
                "parts need one exact Content-Length; chunked bodies are not accepted",
            ),
        });
    }
    let mut values = headers.get_all(header::CONTENT_LENGTH).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(ApiFailure {
            status: StatusCode::LENGTH_REQUIRED,
            error: ProtocolError::new("length_required", "parts need exactly one Content-Length"),
        });
    };
    value
        .to_str()
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .ok_or_else(|| {
            ApiFailure::bad_request("invalid_upload_length", "Content-Length is not a number")
        })
}

fn part_sha256(headers: &HeaderMap) -> Result<String, ApiFailure> {
    let mut values = headers.get_all(PART_SHA256_HEADER).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(ApiFailure::bad_request(
            "invalid_part_sha256",
            "parts need exactly one X-Treer-Part-Sha256 header",
        ));
    };
    let hex = value
        .to_str()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiFailure::bad_request(
            "invalid_part_sha256",
            "X-Treer-Part-Sha256 must be 64 hex characters",
        ));
    }
    Ok(hex)
}

pub(crate) fn status_json(row: &UploadRow, reconciled: bool) -> Value {
    let created = chrono::DateTime::parse_from_rfc3339(&row.created_at).ok();
    let last = chrono::DateTime::parse_from_rfc3339(&row.last_part_at).ok();
    json!({
        "upload_id": row.upload_id,
        "size": row.size,
        "committed_offset": row.committed_offset,
        "part_bytes_max": MACHINE_UPLOAD_PART_MAX_BYTES,
        "part_bytes_min": MACHINE_UPLOAD_PART_MIN_BYTES,
        "state": row.state.as_str(),
        "failure_code": row.failure_code,
        "result": row.result,
        "idempotency_key": row.idempotency_key,
        "finished_at": row.finished_at,
        "terminal": row.state.is_terminal(),
        "expires_at": created.map(|at| crate::upload_store::stamp((at + chrono::Duration::seconds(MACHINE_UPLOAD_MAX_LIFETIME_SECONDS as i64)).with_timezone(&Utc))),
        "idle_expires_at": last.map(|at| crate::upload_store::stamp((at + chrono::Duration::seconds(MACHINE_UPLOAD_IDLE_SECONDS as i64)).with_timezone(&Utc))),
        "reconciled": reconciled,
    })
}

struct Caller {
    workspace_id: String,
    server_id: String,
    subject_kind: &'static str,
    subject_id: String,
}

/// Authentication, target, Policy and the upload subject, shared by every
/// route; nothing here reads a body.
async fn authorize_caller(
    state: &AppState,
    policy: &PolicyEngine,
    session: Option<&CurrentSession>,
    machine: Option<&MachineSession>,
    headers: &HeaderMap,
    workspace_id: &str,
    server_id: &str,
) -> Result<Caller, ApiFailure> {
    state.resolve_server(workspace_id, server_id).await?;
    let subject = control_policy_subject(state, machine, headers, workspace_id).await?;
    require_machine_target(subject.as_ref(), server_id)?;
    authorize_control(
        policy,
        workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_FILE_WRITE,
        PolicyResource::new(RESOURCE_MACHINE, server_id),
    )
    .await?;
    let (kind, id) = control_audit_actor(session, subject.as_ref());
    let Some(id) = id else {
        return Err(ApiFailure::forbidden(
            "identity_required",
            "machine uploads require an authenticated identity",
        ));
    };
    Ok(Caller {
        workspace_id: workspace_id.to_owned(),
        server_id: server_id.to_owned(),
        subject_kind: kind,
        subject_id: id.to_owned(),
    })
}

/// The caller's own upload row, or 404 for anyone else (never 403).
async fn own_row(
    store: &UploadStore,
    caller: &Caller,
    upload_id: &str,
) -> Result<UploadRow, ApiFailure> {
    let row = store
        .get(upload_id)
        .await
        .map_err(store_error)?
        .ok_or_else(not_found)?;
    if !row.same_subject(caller.subject_kind, &caller.subject_id)
        || row.workspace_id != caller.workspace_id
        || row.server_id != caller.server_id
    {
        return Err(not_found());
    }
    Ok(row)
}

async fn audit(auth: &AuthStore, caller: &Caller, action: &str, payload: Value) {
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &caller.workspace_id,
            actor_kind: caller.subject_kind,
            actor_id: Some(&caller.subject_id),
            action,
            resource_kind: "machine",
            resource_id: &caller.server_id,
            resource_name: None,
            payload,
        })
        .await
    {
        tracing::warn!(
            ?error,
            action,
            "failed to record machine upload audit event"
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn begin_machine_upload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<UploadStore>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<MachineUploadBeginRequest>,
) -> Result<Response, ApiFailure> {
    let caller = authorize_caller(
        &state,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace_id,
        &server_id,
    )
    .await?;
    let outcome: Result<Response, ApiFailure> = async {
        let key = idempotency_key(&headers)?;
        validate_begin(&request)?;
        let capabilities = state.server_capabilities(&workspace_id, &server_id).await?;
        if !capabilities.contains(treer_protocol::CAPABILITY_MACHINE_UPLOAD_V3) {
            return Err(conflict(
                "controller_update_required",
                "this machine's Controller does not support chunked uploads yet",
            ));
        }
        let upload_id = format!(
            "upl_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let begun = store
            .begin(BeginParams {
                workspace_id: &workspace_id,
                server_id: &server_id,
                directory: &request.directory,
                file_name: &request.file_name,
                overwrite: request.overwrite,
                size: request.size,
                subject_kind: caller.subject_kind,
                subject_id: &caller.subject_id,
                idempotency_key: &key,
                upload_id: &upload_id,
            })
            .await
            .map_err(store_error)?;
        let row = match begun {
            BeginOutcome::Replayed(row) => {
                return Ok((StatusCode::OK, Json(status_json(&row, false))).into_response());
            }
            BeginOutcome::Conflict => {
                return Err(conflict(
                    "upload_begin_conflict",
                    "this Idempotency-Key was used with different parameters",
                ));
            }
            BeginOutcome::Refused(code) => {
                return Err(too_many(code, "too many uploads for this user, workspace or machine, or begin rate exceeded"));
            }
            BeginOutcome::Created(row) => row,
        };
        if let Err(error) = state
            .send_command(
                &workspace_id,
                &server_id,
                AgentCommand::UploadBegin {
                    upload_id: row.upload_id.clone(),
                    directory: request.directory.clone(),
                    file_name: request.file_name.clone(),
                    overwrite: request.overwrite,
                    size: Some(request.size),
                },
            )
            .await
        {
            store.delete(&row.upload_id).await.map_err(store_error)?;
            return Err(error.into());
        }
        store.activate(&row.upload_id).await.map_err(store_error)?;
        audit(
            &auth,
            &caller,
            "machine.file.upload_begun",
            json!({"upload_id": row.upload_id, "path": format!("{}/{}", request.directory.trim_matches('/'), request.file_name), "size": request.size}),
        )
        .await;
        let row = store.get(&row.upload_id).await.map_err(store_error)?.ok_or_else(not_found)?;
        Ok((StatusCode::CREATED, Json(status_json(&row, false))).into_response())
    }
    .await;
    outcome.or_else(|failure| Ok(closing(failure)))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn list_machine_uploads(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<UploadStore>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Query(query): Query<UploadListQuery>,
) -> Result<Json<Value>, ApiFailure> {
    let caller = authorize_caller(
        &state,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace_id,
        &server_id,
    )
    .await?;
    let rows = store
        .list_own(
            &workspace_id,
            &server_id,
            caller.subject_kind,
            &caller.subject_id,
            query.idempotency_key.as_deref(),
        )
        .await
        .map_err(store_error)?;
    Ok(Json(json!({
        "uploads": rows.iter().map(|row| status_json(row, false)).collect::<Vec<_>>(),
    })))
}

/// Status, reconciled against the Controller for active rows: a Controller
/// offset ahead of the row (a lost part response) is adopted, and a receipt
/// the Controller holds for a committing row finalizes it.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn machine_upload_status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<UploadStore>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id, upload_id)): Path<(String, String, String)>,
) -> Result<Response, ApiFailure> {
    let caller = authorize_caller(
        &state,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace_id,
        &server_id,
    )
    .await?;
    let row = own_row(&store, &caller, &upload_id).await?;
    if !state
        .upload_gate()
        .allow(&format!("status:{upload_id}"), STATUS_PER_UPLOAD_PER_MINUTE)
    {
        return Ok(closing(too_many(
            "machine_upload_rate_limited",
            "status is polled too often for this upload",
        )));
    }
    let (row, reconciled) = reconcile(&state, &auth, &store, &caller, row).await?;
    if !reconciled {
        return Ok(unreconciled(&row));
    }
    Ok(Json(status_json(&row, true)).into_response())
}

/// Adopts the Controller's view when it is ahead of the row.
async fn reconcile(
    state: &AppState,
    auth: &AuthStore,
    store: &UploadStore,
    caller: &Caller,
    row: UploadRow,
) -> Result<(UploadRow, bool), ApiFailure> {
    if !matches!(row.state, UploadState::Active | UploadState::Committing) {
        return Ok((row, true));
    }
    let answer = tokio::time::timeout(
        RECONCILE_TIMEOUT,
        state.send_command(
            &row.workspace_id,
            &row.server_id,
            AgentCommand::UploadResult {
                upload_id: row.upload_id.clone(),
            },
        ),
    )
    .await;
    let Ok(Ok(answer)) = answer else {
        return Ok((row, false));
    };
    match answer["state"].as_str().unwrap_or("unknown") {
        "committed" => {
            let receipt = receipt_from(&answer["result"]);
            store
                .set_committed(&row.upload_id, &receipt)
                .await
                .map_err(store_error)?;
            audit(
                auth,
                caller,
                "machine.file.uploaded",
                json!({"upload_id": row.upload_id, "path": receipt.path, "bytes_written": receipt.bytes_written, "sha256": receipt.sha256, "chunked": true, "recovered": true}),
            )
            .await;
        }
        "active" => {
            // The Controller is the authority for committed bytes: adopt its
            // offset whether the row lags (lost response) or leads (mirror
            // ahead of the truth, which is logged).
            let committed = answer["committed_offset"].as_u64().unwrap_or(0);
            if committed != row.committed_offset && row.state == UploadState::Active {
                if committed < row.committed_offset {
                    tracing::warn!(upload_id = %row.upload_id, row = row.committed_offset, controller = committed, "upload row was ahead of the Controller; mirror truncated");
                }
                let parts: Vec<PartRecord> =
                    serde_json::from_value(answer["parts"].clone()).unwrap_or_default();
                store
                    .record_progress(&row.upload_id, committed, &parts)
                    .await
                    .map_err(store_error)?;
            }
        }
        "failed" => {
            // The Controller recorded a definitive failure receipt (#169):
            // the commit provably did not publish, so the row ends now with
            // the fixed code instead of waiting for outcome_unknown.
            let code = answer["code"]
                .as_str()
                .filter(|code| {
                    !code.is_empty()
                        && code.len() <= 64
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                })
                .unwrap_or("machine_upload_failed");
            store
                .set_terminal(&row.upload_id, UploadState::Failed, code)
                .await
                .map_err(store_error)?;
            audit(
                auth,
                caller,
                "machine.file.upload_aborted",
                json!({"upload_id": row.upload_id, "reason": code}),
            )
            .await;
        }
        _ => {
            if row.state == UploadState::Active {
                // The Controller has neither staging nor receipt (it restarted
                // and swept the upload): the client must start over.
                store
                    .set_terminal(&row.upload_id, UploadState::Failed, "upload_lost")
                    .await
                    .map_err(store_error)?;
                audit(
                    auth,
                    caller,
                    "machine.file.upload_aborted",
                    json!({"upload_id": row.upload_id, "reason": "upload_lost"}),
                )
                .await;
            }
            // A committing row with no Controller evidence stays committing:
            // the outcome is unknown, never a definitive failure; the expiry
            // job ends it as outcome_unknown at the hard lifetime.
        }
    }
    let row = store
        .get(&row.upload_id)
        .await
        .map_err(store_error)?
        .ok_or_else(not_found)?;
    Ok((row, true))
}

fn receipt_from(value: &Value) -> UploadReceipt {
    UploadReceipt {
        path: value["path"].as_str().unwrap_or_default().to_owned(),
        bytes_written: value["bytes_written"].as_u64().unwrap_or(0),
        sha256: value["sha256"].as_str().map(str::to_owned),
        committed_at: crate::upload_store::stamp(Utc::now()),
        cleanup_pending: value["cleanup_pending"].as_bool().unwrap_or(false),
    }
}

/// How long a reconciliation may wait for the Controller before the row is
/// answered as unreconciled.
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(10);

/// A fixed 503 carrying the last known state flagged as unreconciled.
fn unreconciled(row: &UploadRow) -> Response {
    let mut body = status_json(row, false);
    body["error"] = json!({
        "code": "controller_unavailable",
        "message": "the machine's Controller did not answer; the state shown is the last known one",
    });
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&RETRY_AFTER_SECONDS.to_string()).unwrap(),
    );
    response
}

/// Releases the lease when the request ends for any reason, including the
/// handler future being dropped on a client disconnect.
struct LeaseGuard {
    store: UploadStore,
    upload_id: String,
    armed: bool,
}

impl LeaseGuard {
    /// Releases the lease before the response leaves: a client that retries
    /// at once after a 408 or 409 must not meet the previous lease still
    /// being released on a background task (CI race on d9e09813). `Drop`
    /// stays as the fallback for panics and cancelled requests.
    async fn release(&mut self) {
        if self.armed {
            self.armed = false;
            let _ = self.store.release_lease(&self.upload_id).await;
        }
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if self.armed {
            let store = self.store.clone();
            let upload_id = self.upload_id.clone();
            tokio::spawn(async move {
                let _ = store.release_lease(&upload_id).await;
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn put_machine_upload_part(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<UploadStore>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    Path((workspace_id, server_id, upload_id)): Path<(String, String, String)>,
    Query(query): Query<UploadPartQuery>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let headers = request.headers().clone();
    let caller = authorize_caller(
        &state,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace_id,
        &server_id,
    )
    .await?;
    let admitted: Result<Option<AdmittedPart>, ApiFailure> = async {
        let row = own_row(&store, &caller, &upload_id).await?;
        let length = declared_length(&headers)?;
        let sha256 = part_sha256(&headers)?;
        match row.state {
            UploadState::Active => {}
            UploadState::Committing => {
                return Err(conflict("upload_committing", "the upload is being committed"))
            }
            UploadState::Pending => {
                return Err(conflict("upload_not_active", "the upload has not been accepted yet"))
            }
            _ => return Err(conflict("upload_not_active", "the upload has ended")),
        }
        let offset = query.offset;
        let end = offset.checked_add(length).ok_or_else(|| {
            too_large("upload_overrun", "the part extends past the declared size")
        })?;
        if end > row.size {
            return Err(too_large("upload_overrun", "the part extends past the declared size"));
        }
        if length > MACHINE_UPLOAD_PART_MAX_BYTES {
            return Err(too_large(
                "upload_part_too_large",
                &format!("parts may contain at most {MACHINE_UPLOAD_PART_MAX_BYTES} bytes"),
            ));
        }
        if end < row.size && length < MACHINE_UPLOAD_PART_MIN_BYTES {
            return Err(ApiFailure::bad_request(
                "upload_part_too_small",
                &format!("parts other than the last must contain at least {MACHINE_UPLOAD_PART_MIN_BYTES} bytes"),
            ));
        }
        if offset < row.committed_offset {
            // Replay of a committed range: identical => idempotent, no body.
            let known = row
                .parts
                .iter()
                .any(|p| p.offset == offset && p.length == length && p.sha256 == sha256);
            if known && end <= row.committed_offset {
                return Ok(None);
            }
            return Err(conflict(
                "upload_part_conflict",
                "a different part is already committed over this range",
            ));
        }
        if offset > row.committed_offset {
            let mut failure = conflict(
                "upload_offset_gap",
                "the part does not start at the committed offset",
            );
            failure.error.hint = Some(vec![format!("committed_offset={}", row.committed_offset)]);
            return Err(failure);
        }
        if row.parts.len() >= MACHINE_UPLOAD_MAX_PARTS {
            return Err(too_large(
                "upload_too_many_parts",
                &format!("an upload may have at most {MACHINE_UPLOAD_MAX_PARTS} parts"),
            ));
        }
        if !state
            .upload_gate()
            .allow(&format!("parts:{upload_id}"), PARTS_PER_UPLOAD_PER_MINUTE)
        {
            return Err(too_many(
                "machine_upload_rate_limited",
                "parts are sent too often for this upload",
            ));
        }
        let permit = state.upload_gate().acquire_body().ok_or_else(|| {
            too_many(
                "machine_upload_busy",
                "this Proxy is streaming too many parts right now",
            )
        })?;
        let leased = store.acquire_lease(&upload_id).await.map_err(store_error)?;
        let Some(row) = leased else {
            return Err(conflict("upload_busy", "another part of this upload is in flight"));
        };
        let guard = LeaseGuard {
            store: store.clone(),
            upload_id: upload_id.clone(),
            armed: true,
        };
        Ok(Some((row, length, sha256, permit, guard)))
    }
    .await;
    let (row, length, sha256, _permit, mut guard) = match admitted {
        Ok(Some(admitted)) => admitted,
        Ok(None) => {
            let row = own_row(&store, &caller, &upload_id).await?;
            return Ok(Json(json!({"committed_offset": row.committed_offset, "size": row.size, "idempotent": true})).into_response());
        }
        Err(failure) => return Ok(closing(failure)),
    };
    // Under the lease, the Controller's view is adopted before the offset
    // check, so a part the Controller already holds is never refused on the
    // Proxy's stale mirror.
    let (row, reconciled) = match reconcile(&state, &auth, &store, &caller, row).await {
        Ok(reconciled) => reconciled,
        Err(failure) => {
            guard.release().await;
            return Err(failure);
        }
    };
    if !reconciled {
        guard.release().await;
        return Ok(unreconciled(&row));
    }
    if row.state != UploadState::Active {
        guard.release().await;
        return Ok(closing(conflict(
            "upload_not_active",
            "the upload has ended",
        )));
    }
    if query.offset < row.committed_offset {
        let end = query.offset + length;
        let known = row
            .parts
            .iter()
            .any(|p| p.offset == query.offset && p.length == length && p.sha256 == sha256);
        guard.release().await;
        if known && end <= row.committed_offset {
            return Ok(Json(json!({"committed_offset": row.committed_offset, "size": row.size, "idempotent": true})).into_response());
        }
        return Ok(closing(conflict(
            "upload_part_conflict",
            "a different part is already committed over this range",
        )));
    }
    if query.offset != row.committed_offset {
        let mut failure = conflict(
            "upload_offset_gap",
            "the committed offset moved; query status and resume from it",
        );
        failure.error.hint = Some(vec![format!("committed_offset={}", row.committed_offset)]);
        guard.release().await;
        return Ok(closing(failure));
    }
    let outcome = stream_part(&state, &row, query.offset, length, &sha256, request).await;
    match outcome {
        Ok((committed, parts)) => {
            store
                .record_progress(&upload_id, committed, &parts)
                .await
                .map_err(store_error)?;
            guard.armed = false;
            store.release_lease(&upload_id).await.map_err(store_error)?;
            Ok(
                Json(json!({"committed_offset": committed, "size": row.size, "idempotent": false}))
                    .into_response(),
            )
        }
        Err(error) => {
            // Release before answering: the next attempt (a retry after 408
            // idle, a timeout or a transfer error) must find the lease free.
            guard.release().await;
            Ok(closing(error.into()))
        }
    }
}

/// Streams one part body to the Controller in bounded chunks, hashing it, and
/// closes the part; returns the Controller-confirmed committed offset and parts.
async fn stream_part(
    state: &AppState,
    row: &UploadRow,
    offset: u64,
    length: u64,
    sha256: &str,
    request: Request<Body>,
) -> Result<(u64, Vec<PartRecord>), ProtocolError> {
    let started = tokio::time::Instant::now();
    let (idle, total) = (window(&PART_IDLE_MS), window(&PART_TOTAL_MS));
    let deadline = started + total;
    let mut body = request.into_body().into_data_stream();
    let mut buffer: Vec<u8> = Vec::with_capacity(MACHINE_UPLOAD_CHUNK_BYTES.min(length as usize));
    let mut in_flight: VecDeque<tokio::task::JoinHandle<Result<Value, ProtocolError>>> =
        VecDeque::new();
    let mut hasher = sha2::Sha256::new();
    let mut received: u64 = 0;
    let mut position = offset;

    async fn settle(
        in_flight: &mut VecDeque<tokio::task::JoinHandle<Result<Value, ProtocolError>>>,
        keep: usize,
    ) -> Result<(), ProtocolError> {
        while in_flight.len() > keep {
            let handle = in_flight.pop_front().expect("non-empty");
            handle
                .await
                .map_err(|_| ProtocolError::new("machine_upload_failed", "chunk task failed"))??;
        }
        Ok(())
    }
    fn dispatch(
        state: &AppState,
        row: &UploadRow,
        offset: u64,
        chunk: Vec<u8>,
    ) -> tokio::task::JoinHandle<Result<Value, ProtocolError>> {
        let state = state.clone();
        let (workspace_id, server_id, upload_id) = (
            row.workspace_id.clone(),
            row.server_id.clone(),
            row.upload_id.clone(),
        );
        tokio::spawn(async move {
            let content_base64 = base64::engine::general_purpose::STANDARD.encode(&chunk);
            drop(chunk);
            state
                .send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::UploadChunk {
                        upload_id,
                        content_base64,
                        offset: Some(offset),
                    },
                )
                .await
        })
    }

    while received < length {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(ProtocolError::new(
                "machine_upload_timeout",
                "the part did not finish within its time limit",
            ));
        }
        let frame_deadline = (now + idle).min(deadline);
        let frame = match tokio::time::timeout_at(frame_deadline, body.next()).await {
            Ok(Some(Ok(frame))) => frame,
            Ok(Some(Err(_))) => {
                return Err(ProtocolError::new(
                    "machine_upload_body_error",
                    "the part body ended with an error",
                ))
            }
            Ok(None) => break,
            Err(_) => {
                return Err(ProtocolError::new(
                    "machine_upload_idle",
                    "no part data arrived within the idle limit",
                ))
            }
        };
        received = received.saturating_add(frame.len() as u64);
        if received > length {
            return Err(ProtocolError::new(
                "upload_part_length_mismatch",
                "the part body is longer than its Content-Length",
            ));
        }
        hasher.update(&frame);
        let mut frame: &[u8] = &frame;
        while !frame.is_empty() {
            let room = MACHINE_UPLOAD_CHUNK_BYTES - buffer.len();
            let take = room.min(frame.len());
            buffer.extend_from_slice(&frame[..take]);
            frame = &frame[take..];
            #[cfg(test)]
            PART_BUFFERED_PEAK.fetch_max(
                buffer.len() + in_flight.len() * MACHINE_UPLOAD_CHUNK_BYTES + frame.len(),
                std::sync::atomic::Ordering::Relaxed,
            );
            if buffer.len() == MACHINE_UPLOAD_CHUNK_BYTES {
                settle(&mut in_flight, CHUNKS_IN_FLIGHT - 1).await?;
                let chunk =
                    std::mem::replace(&mut buffer, Vec::with_capacity(MACHINE_UPLOAD_CHUNK_BYTES));
                let sent = chunk.len() as u64;
                in_flight.push_back(dispatch(state, row, position, chunk));
                position += sent;
            }
        }
    }
    if received != length {
        return Err(ProtocolError::new(
            "upload_part_length_mismatch",
            "the part body ended before its Content-Length",
        ));
    }
    if !buffer.is_empty() {
        settle(&mut in_flight, CHUNKS_IN_FLIGHT - 1).await?;
        let chunk = std::mem::take(&mut buffer);
        in_flight.push_back(dispatch(state, row, position, chunk));
    }
    settle(&mut in_flight, 0).await?;
    let actual = format!("{:x}", hasher.finalize());
    if actual != sha256 {
        return Err(ProtocolError::new(
            "upload_part_hash_mismatch",
            "the part's SHA-256 does not match X-Treer-Part-Sha256; the part was discarded",
        ));
    }
    let answer = state
        .send_command(
            &row.workspace_id,
            &row.server_id,
            AgentCommand::UploadPartCommit {
                upload_id: row.upload_id.clone(),
                offset,
                length,
                sha256: sha256.to_owned(),
            },
        )
        .await?;
    let committed = answer["committed_offset"]
        .as_u64()
        .unwrap_or(offset + length);
    let mut parts = row.parts.clone();
    parts.push(PartRecord {
        offset,
        length,
        sha256: sha256.to_owned(),
    });
    Ok((committed, parts))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn commit_machine_upload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<UploadStore>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id, upload_id)): Path<(String, String, String)>,
) -> Result<Response, ApiFailure> {
    let caller = authorize_caller(
        &state,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace_id,
        &server_id,
    )
    .await?;
    let row = own_row(&store, &caller, &upload_id).await?;
    let (row, reconciled) = reconcile(&state, &auth, &store, &caller, row).await?;
    if !reconciled {
        return Ok(unreconciled(&row));
    }
    match row.state {
        UploadState::Committed => return Ok(Json(status_json(&row, true)).into_response()),
        UploadState::Failed | UploadState::Aborted | UploadState::Expired => {
            return Ok(closing(conflict(
                row.failure_code.as_deref().unwrap_or("upload_not_active"),
                "the upload ended without a committed file",
            )))
        }
        UploadState::Committing => {
            // Another commit is running or recovering: let the caller poll.
            let (row, _) = reconcile(&state, &auth, &store, &caller, row).await?;
            return Ok(Json(status_json(&row, true)).into_response());
        }
        UploadState::Pending | UploadState::Active => {}
    }
    if row.committed_offset != row.size {
        let mut failure = conflict("upload_incomplete", "not every byte has been committed yet");
        failure.error.hint = Some(vec![format!(
            "committed_offset={} size={}",
            row.committed_offset, row.size
        )]);
        return Ok(closing(failure));
    }
    let Some(row) = store.start_commit(&upload_id).await.map_err(store_error)? else {
        return Ok(closing(conflict(
            "upload_busy",
            "a part of this upload is still in flight",
        )));
    };
    // Detached: the commit runs to completion even if the client disconnects.
    let task = {
        let (state, auth, store, row) = (state.clone(), auth.clone(), store.clone(), row.clone());
        let caller = Caller {
            workspace_id: caller.workspace_id.clone(),
            server_id: caller.server_id.clone(),
            subject_kind: caller.subject_kind,
            subject_id: caller.subject_id.clone(),
        };
        tokio::spawn(async move {
            let outcome = state
                .send_command(
                    &row.workspace_id,
                    &row.server_id,
                    AgentCommand::UploadCommit {
                        upload_id: row.upload_id.clone(),
                    },
                )
                .await;
            match outcome {
                Ok(value) => {
                    let receipt = receipt_from(&value);
                    let _ = store.set_committed(&row.upload_id, &receipt).await;
                    audit(
                        &auth,
                        &caller,
                        "machine.file.uploaded",
                        json!({"upload_id": row.upload_id, "path": receipt.path, "bytes_written": receipt.bytes_written, "sha256": receipt.sha256, "chunked": true}),
                    )
                    .await;
                }
                Err(error) => {
                    // Only a definitive Controller refusal ends the row as
                    // failed; anything else (offline, timeout, generic state
                    // errors) is an unknown outcome and stays committing.
                    let code = match error.code.as_str() {
                        definitive @ ("machine_file_exists"
                        | "machine_upload_size_mismatch"
                        | "machine_upload_publish_unsupported"
                        | "machine_upload_space_insufficient"
                        | "machine_upload_content_mismatch"
                        | "machine_upload_file_mismatch"
                        | "upload_part_hash_mismatch"
                        | "invalid_machine_path"
                        | "invalid_machine_file_name"
                        | "machine_file_denied"
                        | "machine_file_not_found") => definitive.to_owned(),
                        "machine_upload_not_found" => "upload_lost".to_owned(),
                        _ => return,
                    };
                    // A content or length mismatch leaves the staging file
                    // registered on the Controller; free it now rather than
                    // waiting for the idle reaper.
                    if matches!(
                        code.as_str(),
                        "machine_upload_content_mismatch" | "machine_upload_file_mismatch"
                    ) {
                        let _ = state
                            .send_command(
                                &row.workspace_id,
                                &row.server_id,
                                AgentCommand::UploadAbort {
                                    upload_id: row.upload_id.clone(),
                                },
                            )
                            .await;
                    }
                    let _ = store
                        .set_terminal(&row.upload_id, UploadState::Failed, &code)
                        .await;
                    audit(
                        &auth,
                        &caller,
                        "machine.file.upload_aborted",
                        json!({"upload_id": row.upload_id, "reason": code}),
                    )
                    .await;
                }
            }
        })
    };
    let _ = task.await;
    let row = own_row(&store, &caller, &upload_id).await?;
    match row.state {
        UploadState::Committed => Ok(Json(status_json(&row, true)).into_response()),
        UploadState::Failed => Ok(closing(ApiFailure::from(ProtocolError::new(
            row.failure_code
                .clone()
                .unwrap_or_else(|| "machine_upload_failed".into()),
            "the Controller could not publish the upload",
        )))),
        // A slow or unreachable Controller leaves the row committing: the
        // outcome is unknown, never failed; recovery settles it (S4).
        _ => Ok(unreconciled(&row)),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn abort_machine_upload(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<UploadStore>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id, upload_id)): Path<(String, String, String)>,
) -> Result<Response, ApiFailure> {
    let caller = authorize_caller(
        &state,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace_id,
        &server_id,
    )
    .await?;
    let row = own_row(&store, &caller, &upload_id).await?;
    let now = crate::upload_store::stamp(Utc::now());
    match row.state {
        UploadState::Committing => {
            return Ok(closing(conflict(
                "upload_committing",
                "the upload is being committed and cannot be aborted",
            )))
        }
        UploadState::Active | UploadState::Pending if row.lease_held(&now) => {
            return Ok(closing(conflict(
                "upload_busy",
                "a part of this upload is in flight",
            )))
        }
        UploadState::Committed => {
            return Ok(closing(conflict(
                "upload_committed",
                "the upload is already published and cannot be aborted",
            )))
        }
        UploadState::Active | UploadState::Pending => {
            // Take the lease in the same conditional UPDATE a part would, so
            // no part can start between the check and the abort.
            if store
                .acquire_lease(&upload_id)
                .await
                .map_err(store_error)?
                .is_none()
            {
                return Ok(closing(conflict(
                    "upload_busy",
                    "a part of this upload is in flight",
                )));
            }
        }
        _ => return Ok(Json(status_json(&row, true)).into_response()),
    }
    let cleanup = state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::UploadAbort {
                upload_id: upload_id.clone(),
            },
        )
        .await;
    let code = if cleanup.is_ok() {
        "client_abort"
    } else {
        "controller_unavailable"
    };
    store
        .set_terminal(&upload_id, UploadState::Aborted, code)
        .await
        .map_err(store_error)?;
    audit(
        &auth,
        &caller,
        "machine.file.upload_aborted",
        json!({"upload_id": upload_id, "reason": code}),
    )
    .await;
    let row = own_row(&store, &caller, &upload_id).await?;
    let mut body = status_json(&row, cleanup.is_ok());
    body["cleanup_pending"] = json!(cleanup.is_err());
    Ok(Json(body).into_response())
}

/// Periodic expiry (#166): idle or overaged uploads are aborted on the
/// Controller and marked expired; stuck pending rows are dropped; committing
/// rows are recovered from the Controller's receipt; terminal rows past the
/// retention are purged. One pass; the caller schedules it.
pub async fn expire_machine_uploads(state: &AppState, auth: &AuthStore, store: &UploadStore) {
    let candidates = match store
        .expiry_candidates(
            MACHINE_UPLOAD_IDLE_SECONDS as i64,
            MACHINE_UPLOAD_MAX_LIFETIME_SECONDS as i64,
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = ?error.0, "machine upload expiry query failed");
            return;
        }
    };
    for row in candidates {
        let caller = Caller {
            workspace_id: row.workspace_id.clone(),
            server_id: row.server_id.clone(),
            subject_kind: match row.subject_kind.as_str() {
                "user" => "user",
                "agent" => "agent",
                "machine" => "machine",
                _ => "human",
            },
            subject_id: row.subject_id.clone(),
        };
        match row.state {
            UploadState::Committing => {
                let lifetime_over = chrono::DateTime::parse_from_rfc3339(&row.created_at)
                    .ok()
                    .is_some_and(|created| {
                        Utc::now().signed_duration_since(created.with_timezone(&Utc))
                            >= chrono::Duration::seconds(MACHINE_UPLOAD_MAX_LIFETIME_SECONDS as i64)
                    });
                let Ok((row, _)) = reconcile(state, auth, store, &caller, row).await else {
                    continue;
                };
                if row.state == UploadState::Committing && lifetime_over {
                    // No Controller evidence within the lifetime: a distinct
                    // terminal outcome, never a definitive failure.
                    let _ = store
                        .set_terminal(&row.upload_id, UploadState::Failed, "outcome_unknown")
                        .await;
                    audit(
                        auth,
                        &caller,
                        "machine.file.upload_aborted",
                        json!({"upload_id": row.upload_id, "reason": "outcome_unknown"}),
                    )
                    .await;
                }
            }
            UploadState::Pending => {
                let _ = state
                    .send_command(
                        &row.workspace_id,
                        &row.server_id,
                        AgentCommand::UploadAbort {
                            upload_id: row.upload_id.clone(),
                        },
                    )
                    .await;
                let _ = store.delete(&row.upload_id).await;
            }
            _ => {
                let lifetime_over = chrono::DateTime::parse_from_rfc3339(&row.created_at)
                    .ok()
                    .is_some_and(|created| {
                        Utc::now().signed_duration_since(created.with_timezone(&Utc))
                            >= chrono::Duration::seconds(MACHINE_UPLOAD_MAX_LIFETIME_SECONDS as i64)
                    });
                let reason = if lifetime_over {
                    "lifetime_expired"
                } else {
                    "idle_expired"
                };
                let _ = state
                    .send_command(
                        &row.workspace_id,
                        &row.server_id,
                        AgentCommand::UploadAbort {
                            upload_id: row.upload_id.clone(),
                        },
                    )
                    .await;
                let _ = store
                    .set_terminal(&row.upload_id, UploadState::Expired, reason)
                    .await;
                audit(
                    auth,
                    &caller,
                    "machine.file.upload_aborted",
                    json!({"upload_id": row.upload_id, "reason": reason}),
                )
                .await;
            }
        }
    }
    if let Err(error) = store.purge_terminal().await {
        tracing::warn!(error = ?error.0, "machine upload retention purge failed");
    }
}

/// Runs `expire_machine_uploads` every minute for the life of the process.
pub fn spawn_upload_expiry_task(state: AppState, auth: AuthStore, store: UploadStore) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            expire_machine_uploads(&state, &auth, &store).await;
        }
    });
}
