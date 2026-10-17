//! Provider CLI installation routes (#123 part 2). Human-session only: neither
//! the machine router nor the App identity API exposes them. The browser never
//! supplies a command; it chooses a recipe the Controller's embedded table
//! expanded, confirms the displayed plan with a single-use token, and polls a
//! bounded result. Output never enters Proxy storage, audit or logs.
use chrono::Utc;
use serde::de::DeserializeOwned;
use treer_protocol::{
    decide_provider_cli_action, ProviderCliAction, ProviderCliCheckResponse, ProviderCliConsent,
    ProviderCliInstallAccepted, ProviderCliInstallRequest, ProviderCliInstallResponse,
    ProviderCliObservation, ProviderCliOperationReport, ProviderCliOperationResponse,
    ProviderCliOperationStatus, ProviderCliPrepareResponse, ProviderCliRecipeRef,
    ProviderCliRecipesResponse, CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL,
    CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL_V2, PROVIDER_CLI_POLL_AFTER_MS,
};

use super::*;
use crate::auth::{
    ProviderCliBinding, ProviderCliFinishDetails, ProviderCliObservedConsent,
    ProviderCliOperationRecord, ProviderCliScope,
};

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

/// A Controller that installs without observing (v1 only) must be updated;
/// the Proxy never falls back to the unobserved consent flow.
fn controller_outdated() -> ApiFailure {
    ApiFailure {
        status: StatusCode::CONFLICT,
        error: ProtocolError::new(
            "provider_cli_controller_outdated",
            "This machine's Controller predates installation observation; update the machine first",
        ),
    }
}

/// The machine's observation left a process behind: its output is not
/// evidence, and no consent is issued on it.
fn probe_unconfirmed() -> ApiFailure {
    ApiFailure {
        status: StatusCode::CONFLICT,
        error: ProtocolError::new(
            "provider_cli_probe_unconfirmed",
            "The machine's version probe left a process behind, so the existing installation state is not confirmed; nothing was issued",
        ),
    }
}

fn state_unobservable() -> ApiFailure {
    ApiFailure {
        status: StatusCode::CONFLICT,
        error: ProtocolError::new(
            "provider_cli_state_unobservable",
            "The machine could not resolve its npm global prefix, so the existing installation state is unknown; nothing can be installed until a check succeeds",
        ),
    }
}

/// Both provider-CLI capabilities present: the Controller observes before it
/// installs. v1 alone is outdated; neither is unsupported.
async fn require_observing_controller(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
) -> Result<(), ApiFailure> {
    let capabilities = state
        .server_capabilities(workspace_id, server_id)
        .await
        .map_err(controller_failure)?;
    if !capabilities.contains(CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL) {
        return Err(unsupported());
    }
    if !capabilities.contains(CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL_V2) {
        return Err(controller_outdated());
    }
    Ok(())
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
        | "provider_cli_consent_invalid"
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

/// Runs the Controller's observation (its check) and requires the v2 shape.
async fn controller_observation(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
    reference: &ProviderCliRecipeRef,
) -> Result<(ProviderCliCheckResponse, ProviderCliObservation), ApiFailure> {
    let value = state
        .send_command(
            workspace_id,
            server_id,
            AgentCommand::ProviderCliCheck {
                recipe: reference.clone(),
            },
        )
        .await
        .map_err(controller_failure)?;
    let response: ProviderCliCheckResponse = parse_controller(value)?;
    let observation = response
        .observation
        .clone()
        .ok_or_else(controller_outdated)?;
    // The Controller's observation is input, not truth: it is stored, audited
    // and shown, so its shape is checked here and a bad one fails closed.
    validate_observation(&observation, workspace_id, server_id)?;
    Ok((response, observation))
}

fn validate_observation(
    observation: &ProviderCliObservation,
    workspace_id: &str,
    server_id: &str,
) -> Result<(), ApiFailure> {
    if let Err(reason) = observation.validate() {
        tracing::warn!(%workspace_id, %server_id, reason, "Controller returned a malformed provider CLI observation");
        return Err(ApiFailure::bad_gateway(
            "provider_cli_controller_error",
            "Controller returned an unexpected provider CLI response",
        ));
    }
    Ok(())
}

/// Audit payload for an observation: classification only, never the path.
fn observation_payload(observation: &ProviderCliObservation) -> Value {
    json!({
        "observed_state": observation.state.as_str(),
        "observed_version": observation.current_version,
        "observed_in_prefix": observation.found_in_prefix,
        "probe_exit_code": observation.probe.exit_code,
        "probe_timed_out": observation.probe.timed_out,
        "probe_error_code": observation.probe.error_code,
    })
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
/// refuses a changed or unsupported recipe, observes what the install would
/// overwrite, decides the one action the human may consent to, and (unless
/// that action is `noop`) mints a short-lived single-use confirmation token
/// bound to this user, session, recipe, Controller, plan and observation.
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
    require_observing_controller(&state, &workspace_id, &server_id).await?;
    // Admission (rate and live-token ceilings) precedes every Controller
    // command: noop and unobservable prepares make the machine run npm too.
    let scope = scope(&session, &workspace_id, &server_id);
    auth.provider_cli_prepare_begin(&scope).await?;
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
    let (Some(plan), Some(package)) = (
        recipe.plan.as_deref().filter(|_| recipe.supported),
        recipe.package.as_ref(),
    ) else {
        return Err(ApiFailure::bad_request(
            "provider_cli_recipe_unsupported",
            "This recipe cannot be installed on this machine",
        ));
    };
    let target_version = package.version.clone();
    let (_, observation) =
        controller_observation(&state, &workspace_id, &server_id, &reference).await?;
    // #131: a native-binary entry needs the platform's native Node; a wrong
    // or unreadable architecture refuses before anything is fetched. The
    // operation's first step repeats the check on the machine.
    if let Some(expected) = recipe.node_arch.as_deref() {
        if observation.node_arch.as_deref() != Some(expected) {
            let mut payload = observation_payload(&observation);
            payload["outcome"] = Value::String("node_arch_mismatch".into());
            payload["node_arch"] = Value::from(observation.node_arch.clone());
            if let Err(error) = auth
                .provider_cli_audit_outcome(&scope, "provider_cli.prepare", &reference, payload)
                .await
            {
                tracing::warn!(code = error.code(), %workspace_id, %server_id, "failed to record provider CLI prepare audit event");
            }
            return Err(ApiFailure {
                status: StatusCode::CONFLICT,
                error: ProtocolError::new(
                    "provider_cli_node_arch_mismatch",
                    "The machine's Node is not the platform's native architecture, so this recipe cannot be installed there",
                ),
            });
        }
    }
    let observation_hash = observation.hash(&reference.recipe_hash, &target_version);
    let Some(decision) = decide_provider_cli_action(&observation, &target_version) else {
        let unconfirmed = !observation.probe.cleanup_confirmed;
        let mut payload = observation_payload(&observation);
        payload["outcome"] = Value::String(
            if unconfirmed {
                "probe_unconfirmed"
            } else {
                "unobservable"
            }
            .into(),
        );
        if let Err(error) = auth
            .provider_cli_audit_outcome(&scope, "provider_cli.prepare", &reference, payload)
            .await
        {
            tracing::warn!(code = error.code(), %workspace_id, %server_id, "failed to record provider CLI prepare audit event");
        }
        return Err(if unconfirmed {
            probe_unconfirmed()
        } else {
            state_unobservable()
        });
    };
    let mut response = ProviderCliPrepareResponse {
        confirmation_token: None,
        expires_at: None,
        machine_id: server_id.clone(),
        table_hash: recipes.table_hash.clone(),
        recipe: recipe.clone(),
        action: decision.action,
        version_relation: decision.version_relation,
        target_version: target_version.clone(),
        observation: observation.clone(),
        observation_hash: observation_hash.clone(),
    };
    if decision.action == ProviderCliAction::Noop {
        // Nothing would change: no token, no operation, no slot, no count.
        let mut payload = observation_payload(&observation);
        payload["action"] = Value::String("noop".into());
        payload["observation_hash"] = Value::String(observation_hash);
        if let Err(error) = auth
            .provider_cli_audit_outcome(&scope, "provider_cli.prepare", &reference, payload)
            .await
        {
            tracing::warn!(code = error.code(), %workspace_id, %server_id, "failed to record provider CLI prepare audit event");
        }
        return Ok(Json(response));
    }
    let binding = ProviderCliBinding {
        recipe: reference,
        table_hash: recipes.table_hash.clone(),
        controller_source_sha: recipes.controller_source_sha.clone(),
        argv_hash: treer_protocol::plan_argv_hash(plan),
        observed: Some(ProviderCliObservedConsent {
            consent: ProviderCliConsent {
                action: decision.action,
                version_relation: decision.version_relation,
                observation_hash,
            },
            state: observation.state,
            current_version: observation.current_version.clone(),
            found_in_prefix: observation.found_in_prefix,
        }),
    };
    let (confirmation_token, expires_at) = auth.provider_cli_prepare(&scope, &binding).await?;
    response.confirmation_token = Some(confirmation_token);
    response.expires_at = Some(expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    Ok(Json(response))
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
    require_observing_controller(&state, &workspace_id, &server_id).await?;
    let scope = scope(&session, &workspace_id, &server_id);
    let record = auth
        .provider_cli_accept(
            &scope,
            &request.confirmation_token,
            request.acknowledged_relation,
        )
        .await?;
    let consent = record
        .binding
        .observed
        .as_ref()
        .map(|observed| observed.consent.clone())
        .ok_or_else(|| {
            ApiFailure::internal(
                "provider_cli_consent_missing",
                "accepted operation lacks its consent",
            )
        })?;
    let dispatch = state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::ProviderCliInstall {
                operation_id: record.operation_id.clone(),
                recipe: record.binding.recipe.clone(),
                table_hash: record.binding.table_hash.clone(),
                argv_hash: record.binding.argv_hash.clone(),
                consent,
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
            ProviderCliFinishDetails {
                exit_code: report.and_then(|report| report.exit_code),
                argv_hash: report.map(|report| report.argv_hash.as_str()),
                observed_before: report.and_then(|report| report.observed_before.as_ref()),
            },
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
    if let Some(observation) = &response.observation {
        validate_observation(observation, &workspace_id, &server_id)?;
    }
    let mut payload = json!({
        "installed": response.installed,
        "found_in_prefix": response.found_in_prefix,
        "exit_code": response.exit_code,
        "timed_out": response.timed_out,
        "error_code": response.error_code,
    });
    if let Some(observation) = &response.observation {
        payload["observed_state"] = Value::String(observation.state.as_str().to_owned());
        payload["observed_version"] = observation
            .current_version
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
    }
    if let Err(error) = auth
        .provider_cli_audit_outcome(
            &scope(&session, &workspace_id, &server_id),
            "provider_cli.check",
            &reference,
            payload,
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
        action: record
            .binding
            .observed
            .as_ref()
            .map(|observed| observed.consent.action),
        version_relation: record
            .binding
            .observed
            .as_ref()
            .and_then(|observed| observed.consent.version_relation),
        observation_hash: record
            .binding
            .observed
            .as_ref()
            .map(|observed| observed.consent.observation_hash.clone()),
        observed_state: record
            .binding
            .observed
            .as_ref()
            .map(|observed| observed.state),
        observed_version: record
            .binding
            .observed
            .as_ref()
            .and_then(|observed| observed.current_version.clone()),
        observed_before: report.and_then(|report| report.observed_before.clone()),
    }
}
