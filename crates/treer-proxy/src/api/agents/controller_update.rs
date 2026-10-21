//! Human-only #222 adapter. The install-CLI scope/Policy gate is reused; typed
//! Controller commands never pass through Exec or a replayable mutation outbox.
use super::*;
use crate::auth::{update_not_found, ControllerUpdateScope};
use axum::body::Bytes;
use chrono::Utc;
use serde::de::DeserializeOwned;
use treer_protocol::controller_update as wire;
use treer_protocol::controller_update_runtime::{DispatchBinding, Inspection};

fn scope<'a>(
    session: &'a CurrentSession,
    ws: &'a str,
    machine: &'a str,
) -> ControllerUpdateScope<'a> {
    ControllerUpdateScope {
        workspace_id: ws,
        server_id: machine,
        user_id: &session.user_id,
        session_token: &session.token,
    }
}
fn failure(code: &str) -> ApiFailure {
    ApiFailure {status:StatusCode::CONFLICT,error:ProtocolError::new(code,"Controller update is unavailable; refresh the operation state before taking another action")}
}
fn unsupported() -> ApiFailure {
    ApiFailure {
        status: StatusCode::NOT_IMPLEMENTED,
        error: ProtocolError::new(
            "controller_update_unsupported",
            "This machine requires a manual Controller update",
        ),
    }
}
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiFailure> {
    if bytes.len() > 1024 {
        return Err(failure("controller_update_request_invalid"));
    }
    serde_json::from_slice(bytes).map_err(|_| failure("controller_update_request_invalid"))
}
fn response<T: serde::Serialize>(body: T) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}
fn report<T: DeserializeOwned>(data: serde_json::Value) -> Result<T, ApiFailure> {
    if serde_json::to_vec(&data)
        .map_err(|_| failure("controller_update_receipt_invalid"))?
        .len()
        > 64 * 1024
    {
        return Err(failure("controller_update_receipt_invalid"));
    }
    serde_json::from_value(data).map_err(|_| failure("controller_update_receipt_invalid"))
}
async fn authorize(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    session: &CurrentSession,
    ws: &str,
    machine: &str,
) -> Result<(), ApiFailure> {
    super::provider_cli::provider_cli_scope(state, auth, policy, session, ws, machine).await?;
    // A machine name is not a stable update scope or Policy resource identity.
    if state.resolve_server(ws, machine).await?.server_id != machine {
        return Err(failure("controller_update_request_invalid"));
    }
    Ok(())
}
async fn inspection(state: &AppState, ws: &str, machine: &str) -> Result<Inspection, ApiFailure> {
    let caps = state
        .server_capabilities(ws, machine)
        .await
        .map_err(|_| failure("controller_update_offline"))?;
    if !caps.contains(wire::CAPABILITY) {
        return Err(unsupported());
    }
    let data = tokio::time::timeout(
        Duration::from_secs(10),
        state.send_command(ws, machine, AgentCommand::ControllerUpdateInspect {}),
    )
    .await
    .map_err(|_| failure("controller_update_offline"))?
    .map_err(|_| unsupported())?;
    let report: Inspection = report(data)?;
    if report.current.validate().is_err()
        || report.observed_at > Utc::now() + chrono::Duration::seconds(30)
        || report.observed_at < Utc::now() - chrono::Duration::seconds(30)
    {
        return Err(failure("controller_update_receipt_invalid"));
    }
    Ok(report)
}
async fn catalog(
    state: &AppState,
    auth: &AuthStore,
    s: &ControllerUpdateScope<'_>,
) -> Result<wire::Catalog, ApiFailure> {
    let report = inspection(state, s.workspace_id, s.server_id).await?;
    let (revision, targets) = auth
        .controller_update_targets(report.platform, &report.current)
        .await?;
    let busy = auth.controller_update_busy(s).await?;
    let support = if report.native_ready {
        wire::Support::Supported
    } else {
        wire::Support::ManualUpdateRequired
    };
    let availability = if busy {
        wire::Availability::Busy
    } else {
        wire::Availability::Ready
    };
    let result = wire::Catalog {
        support,
        availability: if !report.native_ready {
            wire::Availability::CatalogUnavailable
        } else {
            availability
        },
        current: Some(report.current),
        observed_at: Some(report.observed_at),
        platform: report.platform,
        catalog_revision: revision,
        targets: if busy || !report.native_ready {
            vec![]
        } else {
            targets
        },
    };
    result
        .validate()
        .map_err(|_| failure("controller_update_catalog_unavailable"))?;
    Ok(result)
}

pub(crate) async fn controller_update_catalog(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((ws, machine)): Path<(String, String)>,
) -> Result<Response, ApiFailure> {
    authorize(&state, &auth, &policy, &session, &ws, &machine).await?;
    Ok(response(
        catalog(&state, &auth, &scope(&session, &ws, &machine)).await?,
    ))
}
pub(crate) async fn controller_update_prepare(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((ws, machine)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    authorize(&state, &auth, &policy, &session, &ws, &machine).await?;
    let request: wire::PrepareRequest = decode(&body)?;
    request
        .validate()
        .map_err(|_| failure("controller_update_request_invalid"))?;
    let s = scope(&session, &ws, &machine);
    auth.controller_update_prepare_begin(&s).await?;
    let catalog = catalog(&state, &auth, &s).await?;
    if catalog.availability != wire::Availability::Ready
        || catalog.support != wire::Support::Supported
    {
        return Err(failure("controller_update_busy"));
    }
    let target = catalog
        .targets
        .into_iter()
        .find(|t| t.target_id == request.target_id)
        .ok_or_else(|| failure("controller_update_target_unavailable"))?;
    let current = catalog
        .current
        .ok_or_else(|| failure("controller_update_preview_changed"))?;
    let digest = crate::auth::sha256_hex(
        &serde_json::to_string(&(
            &current,
            &target,
            catalog.catalog_revision,
            wire::PLAN_STEPS,
        ))
        .map_err(|_| failure("controller_update_preview_changed"))?,
    );
    let binding = DispatchBinding {
        operation_id: Uuid::new_v4().to_string(),
        workspace_id: ws.clone(),
        server_id: machine.clone(),
        current,
        target,
        catalog_revision: catalog.catalog_revision,
        plan_digest: digest,
        created_at: chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis())
            .ok_or_else(|| failure("controller_update_preview_changed"))?,
    };
    Ok(response(
        auth.controller_update_preview(&s, &binding).await?,
    ))
}
pub(crate) async fn controller_update_confirm(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((ws, machine)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response, ApiFailure> {
    authorize(&state, &auth, &policy, &session, &ws, &machine).await?;
    let request: wire::ConfirmRequest = decode(&body)?;
    request
        .validate()
        .map_err(|_| failure("controller_update_request_invalid"))?;
    let s = scope(&session, &ws, &machine);
    let binding = auth
        .controller_update_confirmation(&s, &request.confirmation_token)
        .await?;
    // Consumed-token lookup must work offline and while the slot is held.
    match auth
        .controller_update_operation(&s, &binding.operation_id)
        .await
    {
        Ok(existing) => return Ok(response(existing)),
        Err(e) if e.code() == "controller_update_operation_not_found" => {}
        Err(e) => return Err(e.into()),
    }
    let fresh = catalog(&state, &auth, &s).await?;
    if fresh.support != wire::Support::Supported
        || fresh.availability != wire::Availability::Ready
        || fresh.current.as_ref() != Some(&binding.current)
        || fresh.catalog_revision != binding.catalog_revision
        || !fresh.targets.contains(&binding.target)
    {
        return Err(failure("controller_update_preview_changed"));
    }
    let accepted = auth
        .controller_update_accept(&s, &request.confirmation_token, &binding)
        .await?;
    if let Some(binding) = accepted.dispatch {
        // Exactly one call after durable acceptance. A timeout/drop/connection
        // replacement leaves the slot held and is reconciled only by job GET.
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            state.send_command(
                &ws,
                &machine,
                AgentCommand::ControllerUpdateDispatch { binding },
            ),
        )
        .await;
    }
    Ok(response(accepted.operation))
}
pub(crate) async fn controller_update_operations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((ws, machine)): Path<(String, String)>,
) -> Result<Response, ApiFailure> {
    authorize(&state, &auth, &policy, &session, &ws, &machine).await?;
    Ok(response(
        auth.controller_update_discovery(&scope(&session, &ws, &machine))
            .await?,
    ))
}
pub(crate) async fn controller_update_operation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((ws, machine, id)): Path<(String, String, String)>,
) -> Result<Response, ApiFailure> {
    authorize(&state, &auth, &policy, &session, &ws, &machine).await?;
    if Uuid::parse_str(&id).is_err()
        || Uuid::parse_str(&id)
            .ok()
            .is_none_or(|v| v.to_string() != id || v.get_version_num() != 4)
    {
        return Err(update_not_found().into());
    }
    let s = scope(&session, &ws, &machine);
    let mut op = auth.controller_update_note_silence(&s, &id).await?;
    if auth.controller_update_query_lease(&s, &id).await? {
        if let Ok(Ok(data)) = tokio::time::timeout(
            Duration::from_secs(10),
            state.send_command(
                &ws,
                &machine,
                AgentCommand::ControllerUpdateOperation { operation_id: id },
            ),
        )
        .await
        {
            let report: wire::Operation = report(data)?;
            op = auth.controller_update_observe(&s, &report).await?;
        }
    }
    Ok(response(op))
}

// Applied to all route outcomes, including body-limit and handler refusals.
pub(crate) async fn controller_update_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
