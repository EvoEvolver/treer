//! Provider CLI installation routes (#123 part 2). Human-session only: neither
//! the machine router nor the App identity API exposes them. The browser never
//! supplies a command; it chooses a recipe the Controller's embedded table
//! expanded, confirms the displayed plan with a single-use token, and polls a
//! bounded result. Output never enters Proxy storage, audit or logs.
use chrono::Utc;
use serde::de::DeserializeOwned;
use treer_protocol::{
    ProviderCliCheckResponse, ProviderCliInstallAccepted, ProviderCliInstallRequest,
    ProviderCliInstallResponse, ProviderCliOperationReport, ProviderCliOperationResponse,
    ProviderCliOperationStatus, ProviderCliPrepareResponse, ProviderCliRecipeRef,
    ProviderCliRecipesResponse, CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL,
    PROVIDER_CLI_POLL_AFTER_MS,
};

use super::*;
use crate::auth::{ProviderCliBinding, ProviderCliOperationRecord, ProviderCliScope};

async fn provider_cli_scope(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    session: &CurrentSession,
    workspace_id: &str,
    server_id: &str,
) -> Result<(), ApiFailure> {
    state.resolve_server(workspace_id, server_id).await?;
    auth.require_workspace_member(workspace_id, &session.user_id)
        .await?;
    authorize_control(
        policy,
        workspace_id,
        Some(&PolicySubject::Human {
            user_id: session.user_id.clone(),
        }),
        ACTION_MACHINE_EXEC,
        PolicyResource::new(RESOURCE_MACHINE, server_id),
    )
    .await
}

fn scope<'a>(
    session: &'a CurrentSession,
    workspace_id: &'a str,
    server_id: &'a str,
) -> ProviderCliScope<'a> {
    ProviderCliScope {
        workspace_id,
        server_id,
        user_id: &session.user_id,
        session_token: &session.token,
    }
}

fn unsupported() -> ApiFailure {
    ApiFailure {
        status: StatusCode::NOT_IMPLEMENTED,
        error: ProtocolError::new(
            "provider_cli_unsupported",
            "Controller does not support provider CLI installation",
        ),
    }
}

fn machine_offline() -> ApiFailure {
    ApiFailure::service_unavailable(
        "provider_cli_machine_offline",
        "Machine is offline; installation state is retained on the machine",
    )
}

fn output_expired() -> ApiFailure {
    ApiFailure {
        status: StatusCode::GONE,
        error: ProtocolError::new(
            "provider_cli_output_expired",
            "Installation result is no longer retained",
        ),
    }
}

/// Controller and transport errors reach the browser as fixed codes; raw
/// diagnostics, paths and output never do.
fn controller_failure(error: ProtocolError) -> ApiFailure {
    let status = match error.code.as_str() {
        "unsupported_command" => return unsupported(),
        "server_offline" => return machine_offline(),
        "provider_cli_operation_not_found" => return output_expired(),
        "command_timeout" | "command_cancelled" | "provider_cli_dispatch_stale" => {
            return ApiFailure::service_unavailable(
                "provider_cli_unavailable",
                "Controller did not answer; retry the request",
            )
        }
        "provider_cli_table_mismatch"
        | "provider_cli_recipe_mismatch"
        | "provider_cli_machine_busy"
        | "provider_cli_operation_conflict" => StatusCode::CONFLICT,
        "provider_cli_recipe_unsupported"
        | "provider_cli_recipe_invalid"
        | "provider_cli_platform_unsupported" => StatusCode::BAD_REQUEST,
        "provider_cli_table_invalid" | "provider_cli_storage_exhausted" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => {
            return ApiFailure::bad_gateway(
                "provider_cli_controller_error",
                "Controller rejected the provider CLI request",
            )
        }
    };
    ApiFailure {
        status,
        error: ProtocolError::new(&error.code, &error.message),
    }
}

fn parse_controller<T: DeserializeOwned>(value: Value) -> Result<T, ApiFailure> {
    serde_json::from_value(value).map_err(|_| {
        ApiFailure::bad_gateway(
            "provider_cli_controller_error",
            "Controller returned an unexpected provider CLI response",
        )
    })
}

fn validate_recipe_ref(reference: &ProviderCliRecipeRef) -> Result<(), ApiFailure> {
    let id = reference.recipe_id.as_bytes();
    let id_ok = (2..=80).contains(&id.len())
        && id[0].is_ascii_lowercase()
        && id
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
    let hash_ok = reference.recipe_hash.len() == 64
        && reference
            .recipe_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !id_ok || !hash_ok || reference.recipe_version == 0 {
        return Err(ApiFailure::bad_request(
            "provider_cli_recipe_invalid",
            "recipe_id, recipe_version and recipe_hash must identify one table entry",
        ));
    }
    Ok(())
}

async fn controller_recipes(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
) -> Result<ProviderCliRecipesResponse, ApiFailure> {
    let value = state
        .send_command(workspace_id, server_id, AgentCommand::ProviderCliRecipes {})
        .await
        .map_err(controller_failure)?;
    parse_controller(value)
}

/// `GET .../provider-clis/recipes`: the machine-scoped table with the exact
/// plan each supported entry would run; unsupported entries carry a reason.
pub(crate) async fn provider_cli_recipes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<ProviderCliRecipesResponse>, ApiFailure> {
    provider_cli_scope(&state, &auth, &policy, &session, &workspace_id, &server_id).await?;
    Ok(Json(
        controller_recipes(&state, &workspace_id, &server_id).await?,
    ))
}

/// `POST .../provider-clis/prepare`: re-reads the Controller's current table,
/// refuses a changed or unsupported recipe, and mints a short-lived single-use
/// confirmation token bound to this user, session, recipe and Controller.
pub(crate) async fn provider_cli_prepare(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(reference): Json<ProviderCliRecipeRef>,
) -> Result<Json<ProviderCliPrepareResponse>, ApiFailure> {
    provider_cli_scope(&state, &auth, &policy, &session, &workspace_id, &server_id).await?;
    validate_recipe_ref(&reference)?;
    let recipes = controller_recipes(&state, &workspace_id, &server_id).await?;
    let recipe = recipes
        .recipes
        .iter()
        .find(|recipe| {
            recipe.recipe_id == reference.recipe_id
                && recipe.recipe_version == reference.recipe_version
                && recipe.recipe_hash == reference.recipe_hash
        })
        .ok_or_else(|| ApiFailure {
            status: StatusCode::CONFLICT,
            error: ProtocolError::new(
                "provider_cli_recipe_mismatch",
                "Recipe is unknown or changed on this machine; reload the recipes",
            ),
        })?;
    let Some(plan) = recipe.plan.as_deref().filter(|_| recipe.supported) else {
        return Err(ApiFailure::bad_request(
            "provider_cli_recipe_unsupported",
            "This recipe cannot be installed on this machine",
        ));
    };
    let binding = ProviderCliBinding {
        recipe: reference,
        table_hash: recipes.table_hash.clone(),
        controller_source_sha: recipes.controller_source_sha.clone(),
        argv_hash: treer_protocol::plan_argv_hash(plan),
    };
    let (confirmation_token, expires_at) = auth
        .provider_cli_prepare(&scope(&session, &workspace_id, &server_id), &binding)
        .await?;
    Ok(Json(ProviderCliPrepareResponse {
        confirmation_token,
        expires_at: expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        machine_id: server_id,
        table_hash: recipes.table_hash,
        recipe: recipe.clone(),
    }))
}

/// `POST .../provider-clis/install`: consumes the token and inserts the unique
/// queued operation in one PostgreSQL transaction, then dispatches exactly once.
/// Capability and online state are checked before consumption so a refused
/// request leaves the confirmation usable; a lost dispatch becomes `unknown`.
pub(crate) async fn provider_cli_install(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<ProviderCliInstallRequest>,
) -> Result<Json<ProviderCliInstallResponse>, ApiFailure> {
    provider_cli_scope(&state, &auth, &policy, &session, &workspace_id, &server_id).await?;
    let capabilities = state
        .server_capabilities(&workspace_id, &server_id)
        .await
        .map_err(controller_failure)?;
    if !capabilities.contains(CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL) {
        return Err(unsupported());
    }
    let scope = scope(&session, &workspace_id, &server_id);
    let record = auth
        .provider_cli_accept(&scope, &request.confirmation_token)
        .await?;
    let dispatch = state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::ProviderCliInstall {
                operation_id: record.operation_id.clone(),
                recipe: record.binding.recipe.clone(),
                table_hash: record.binding.table_hash.clone(),
                argv_hash: record.binding.argv_hash.clone(),
            },
        )
        .await;
    let status = match dispatch {
        Ok(value) => {
            let accepted: ProviderCliInstallAccepted = match parse_controller(value) {
                Ok(accepted) => accepted,
                Err(failure) => {
                    finish(
                        &auth,
                        &scope,
                        &record,
                        ProviderCliOperationStatus::Unknown,
                        "dispatch_unconfirmed",
                        None,
                    )
                    .await;
                    return Err(failure);
                }
            };
            if accepted.operation_id != record.operation_id
                || accepted.argv_hash != record.binding.argv_hash
            {
                finish(
                    &auth,
                    &scope,
                    &record,
                    ProviderCliOperationStatus::Unknown,
                    "dispatch_unconfirmed",
                    None,
                )
                .await;
                return Err(ApiFailure::bad_gateway(
                    "provider_cli_controller_error",
                    "Controller acknowledged a different operation",
                ));
            }
            auth.provider_cli_mark_dispatched(&record.operation_id, &accepted.argv_hash)
                .await?;
            ProviderCliOperationStatus::Running
        }
        Err(error) => match error.code.as_str() {
            // No frame reached the Controller: the job never started.
            "server_offline" => {
                finish(
                    &auth,
                    &scope,
                    &record,
                    ProviderCliOperationStatus::Failed,
                    "machine_offline",
                    None,
                )
                .await;
                return Err(machine_offline());
            }
            "unsupported_command" => {
                finish(
                    &auth,
                    &scope,
                    &record,
                    ProviderCliOperationStatus::Failed,
                    "controller_unsupported",
                    None,
                )
                .await;
                return Err(unsupported());
            }
            // The frame may have been delivered; nothing is replayed.
            "command_timeout" | "command_cancelled" | "provider_cli_dispatch_stale" => {
                finish(
                    &auth,
                    &scope,
                    &record,
                    ProviderCliOperationStatus::Unknown,
                    "dispatch_unconfirmed",
                    None,
                )
                .await;
                ProviderCliOperationStatus::Unknown
            }
            code => {
                // The Controller refused before running anything.
                let code = code.to_owned();
                finish(
                    &auth,
                    &scope,
                    &record,
                    ProviderCliOperationStatus::Failed,
                    &code,
                    None,
                )
                .await;
                return Err(controller_failure(error));
            }
        },
    };
    Ok(Json(ProviderCliInstallResponse {
        operation_id: record.operation_id,
        machine_id: server_id,
        status,
        poll_after_ms: PROVIDER_CLI_POLL_AFTER_MS,
    }))
}

async fn finish(
    auth: &AuthStore,
    scope: &ProviderCliScope<'_>,
    record: &ProviderCliOperationRecord,
    status: ProviderCliOperationStatus,
    error_code: &str,
    report: Option<&ProviderCliOperationReport>,
) {
    let result = auth
        .provider_cli_finish(
            scope,
            &record.operation_id,
            status,
            Some(error_code).filter(|code| !code.is_empty()),
            report.and_then(|report| report.exit_code),
            report.map(|report| report.argv_hash.as_str()),
        )
        .await;
    if let Err(error) = result {
        tracing::warn!(code = error.code(), operation_id = %record.operation_id, "failed to record provider CLI operation outcome");
    }
}

/// `POST .../provider-clis/check`: runs only the recipe's vetted version probe,
/// resolved from the installed global prefix, never PATH. Nothing is installed.
pub(crate) async fn provider_cli_check(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(reference): Json<ProviderCliRecipeRef>,
) -> Result<Json<ProviderCliCheckResponse>, ApiFailure> {
    provider_cli_scope(&state, &auth, &policy, &session, &workspace_id, &server_id).await?;
    validate_recipe_ref(&reference)?;
    let value = state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::ProviderCliCheck {
                recipe: reference.clone(),
            },
        )
        .await
        .map_err(controller_failure)?;
    let response: ProviderCliCheckResponse = parse_controller(value)?;
    if let Err(error) = auth
        .provider_cli_audit_check(
            &scope(&session, &workspace_id, &server_id),
            &reference,
            json!({
                "installed": response.installed,
                "found_in_prefix": response.found_in_prefix,
                "exit_code": response.exit_code,
                "timed_out": response.timed_out,
                "error_code": response.error_code,
            }),
        )
        .await
    {
        tracing::warn!(code = error.code(), %workspace_id, %server_id, "failed to record provider CLI check audit event");
    }
    Ok(Json(response))
}

/// `GET .../provider-clis/operations/{id}`: creator-only polling. Terminal
/// output comes from the Controller's private store; the Proxy records only the
/// status transition. Silence past the grace period becomes `unknown`.
pub(crate) async fn provider_cli_operation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, server_id, operation_id)): Path<(String, String, String)>,
) -> Result<Json<ProviderCliOperationResponse>, ApiFailure> {
    provider_cli_scope(&state, &auth, &policy, &session, &workspace_id, &server_id).await?;
    let scope = scope(&session, &workspace_id, &server_id);
    let mut record = auth.provider_cli_operation(&scope, &operation_id).await?;
    if record.workspace_id != workspace_id || record.created_by != session.user_id {
        return Err(ApiFailure::not_found(
            "provider_cli_operation_not_found",
            "Installation operation not available",
        ));
    }
    let now = Utc::now();
    // Durable status comes before output retention: a silent Controller makes
    // the row unknown (and frees the slot) even when the output has expired.
    if record.controller_silent_past_grace(now) {
        auth.provider_cli_reconcile_stale(&scope).await?;
        record = auth.provider_cli_operation(&scope, &operation_id).await?;
    }
    if record.expired(now) {
        return Err(output_expired());
    }
    let report = state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::ProviderCliOperation {
                operation_id: operation_id.clone(),
            },
        )
        .await;
    let report = match report {
        Ok(value) => {
            let report: ProviderCliOperationReport = parse_controller(value)?;
            if report.operation_id != operation_id {
                return Err(ApiFailure::bad_gateway(
                    "provider_cli_controller_error",
                    "Controller reported a different operation",
                ));
            }
            if report.status.is_terminal() && record.status != report.status {
                // Also resolves a Proxy-side `unknown` with the machine's own
                // observation; the finished audit row records the resolution.
                let code = report.error_code.clone().unwrap_or_default();
                finish(&auth, &scope, &record, report.status, &code, Some(&report)).await;
                record = auth.provider_cli_operation(&scope, &operation_id).await?;
            } else if !report.status.is_terminal() {
                auth.provider_cli_touch(&operation_id).await?;
                record.reported_at = Some(now);
                record.status = ProviderCliOperationStatus::Running;
            }
            Some(report)
        }
        // An unknown row has no output to fetch; its status is the answer even
        // when the Controller is offline, old or has no record.
        Err(_) if record.status == ProviderCliOperationStatus::Unknown => None,
        Err(error) if record.status.is_terminal() => {
            return Err(match error.code.as_str() {
                "provider_cli_operation_not_found" => output_expired(),
                _ => controller_failure(error),
            });
        }
        Err(error) => {
            match error.code.as_str() {
                "provider_cli_operation_not_found" => {
                    finish(
                        &auth,
                        &scope,
                        &record,
                        ProviderCliOperationStatus::Unknown,
                        "dispatch_unconfirmed",
                        None,
                    )
                    .await;
                    record = auth.provider_cli_operation(&scope, &operation_id).await?;
                }
                "unsupported_command" if record.controller_silent_past_grace(now) => {
                    finish(
                        &auth,
                        &scope,
                        &record,
                        ProviderCliOperationStatus::Unknown,
                        "controller_unreachable",
                        None,
                    )
                    .await;
                    record = auth.provider_cli_operation(&scope, &operation_id).await?;
                }
                _ if record.controller_silent_past_grace(now) => {
                    finish(
                        &auth,
                        &scope,
                        &record,
                        ProviderCliOperationStatus::Unknown,
                        "controller_unreachable",
                        None,
                    )
                    .await;
                    record = auth.provider_cli_operation(&scope, &operation_id).await?;
                }
                _ => {}
            }
            None
        }
    };
    Ok(Json(operation_response(&record, report.as_ref(), now)))
}

fn operation_response(
    record: &ProviderCliOperationRecord,
    report: Option<&ProviderCliOperationReport>,
    now: chrono::DateTime<Utc>,
) -> ProviderCliOperationResponse {
    let terminal = record.status.is_terminal();
    let output = report.filter(|report| terminal && report.status.is_terminal());
    ProviderCliOperationResponse {
        operation_id: record.operation_id.clone(),
        machine_id: record.server_id.clone(),
        recipe_id: record.binding.recipe.recipe_id.clone(),
        recipe_version: record.binding.recipe.recipe_version,
        recipe_hash: record.binding.recipe.recipe_hash.clone(),
        status: record.status,
        step: report.and_then(|report| report.step),
        elapsed_ms: report.map(|report| report.elapsed_ms).unwrap_or_else(|| {
            let end = record.finished_at.unwrap_or(now);
            u64::try_from((end - record.created_at).num_milliseconds()).unwrap_or_default()
        }),
        exit_code: record
            .exit_code
            .or_else(|| output.and_then(|report| report.exit_code)),
        timed_out: record.status == ProviderCliOperationStatus::TimedOut
            || output.is_some_and(|report| report.timed_out),
        stdout: output
            .map(|report| report.stdout.clone())
            .unwrap_or_default(),
        stderr: output
            .map(|report| report.stderr.clone())
            .unwrap_or_default(),
        truncated: output.is_some_and(|report| report.truncated),
        error_code: record.error_code.clone(),
        steps: output.and_then(|report| report.steps.clone()),
        plan: output.and_then(|report| report.plan.clone()),
        poll_after_ms: (!terminal).then_some(PROVIDER_CLI_POLL_AFTER_MS),
    }
}
