use super::*;
use axum::extract::RawQuery;
use chrono::{DateTime, Utc};
use std::sync::{Mutex, OnceLock};
use treer_protocol::{MachineMetricsQuery, MachineMetricsResponse};

const ACTION: &str = "machine.metrics.read";
static ADMITTED: OnceLock<Mutex<HashSet<(String, String, String)>>> = OnceLock::new();

struct Admission((String, String, String));
struct AdmittedBytes {
    bytes: Vec<u8>,
    _admission: Admission,
}
impl AsRef<[u8]> for AdmittedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        if let Ok(mut active) = ADMITTED.get_or_init(Mutex::default).lock() {
            active.remove(&self.0);
        }
    }
}
impl Admission {
    fn take(workspace: &str, machine: &str, actor: &str) -> Result<Self, ApiFailure> {
        let key = (workspace.into(), machine.into(), actor.into());
        let mut active = ADMITTED
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| metrics_error("machine_metrics_busy"))?;
        if active.len() >= 2
            || active.contains(&key)
            || active
                .iter()
                .filter(|(w, m, _)| w == workspace && m == machine)
                .count()
                >= 4
        {
            return Err(metrics_error("machine_metrics_busy"));
        }
        active.insert(key.clone());
        Ok(Self(key))
    }
}

fn metrics_error(code: &str) -> ApiFailure {
    let status = match code {
        "machine_metrics_invalid_range" | "machine_metrics_invalid_query" => {
            StatusCode::BAD_REQUEST
        }
        "machine_metrics_forbidden" => StatusCode::FORBIDDEN,
        "machine_metrics_offline" => StatusCode::SERVICE_UNAVAILABLE,
        "machine_metrics_unsupported" => StatusCode::NOT_IMPLEMENTED,
        "machine_metrics_disabled" => StatusCode::CONFLICT,
        "machine_metrics_busy" => StatusCode::TOO_MANY_REQUESTS,
        "machine_metrics_timeout" => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::BAD_GATEWAY,
    };
    ApiFailure {
        status,
        error: ProtocolError::new(code, "Machine metrics query refused"),
    }
}

fn parse_query(raw: Option<&str>, now: DateTime<Utc>) -> Result<MachineMetricsQuery, ApiFailure> {
    let mut from = None;
    let mut to = None;
    let mut max_points = None;
    if raw.is_some_and(|raw| raw.len() > 512) {
        return Err(metrics_error("machine_metrics_invalid_query"));
    }
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        match key.as_ref() {
            "from" if from.is_none() => {
                from = Some(
                    value
                        .parse::<DateTime<Utc>>()
                        .map_err(|_| metrics_error("machine_metrics_invalid_range"))?,
                )
            }
            "to" if to.is_none() => {
                to = Some(
                    value
                        .parse::<DateTime<Utc>>()
                        .map_err(|_| metrics_error("machine_metrics_invalid_range"))?,
                )
            }
            "max_points"
                if max_points.is_none()
                    && !value.is_empty()
                    && value.bytes().all(|c| c.is_ascii_digit()) =>
            {
                max_points = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| metrics_error("machine_metrics_invalid_query"))?,
                );
            }
            _ => return Err(metrics_error("machine_metrics_invalid_query")),
        }
    }
    let (from, to) = match (from, to) {
        (None, None) => (now - chrono::Duration::minutes(15), now),
        (Some(from), Some(to)) => (from, to),
        _ => return Err(metrics_error("machine_metrics_invalid_range")),
    };
    let query = MachineMetricsQuery {
        from,
        to,
        max_points: max_points.unwrap_or(360),
    };
    if !query.valid_at(now) {
        return Err(metrics_error("machine_metrics_invalid_range"));
    }
    Ok(query)
}

/// Both human and Agent routes use the existing membership, identity and
/// machine-resource Policy boundary. No exec/file permission is substituted.
#[allow(clippy::too_many_arguments)] // Independent Axum identity/authz extractors.
pub(crate) async fn read_machine_metrics(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Response {
    let result = async {
        let server_id = state
            .resolve_server(&workspace_id, &server_id)
            .await?
            .server_id;
        let subject = control_policy_subject(
            &state,
            machine.as_ref().map(|v| &v.0),
            &headers,
            &workspace_id,
        )
        .await?
        .or_else(|| {
            session
                .as_ref()
                .map(|session| crate::policy::PolicySubject::Human {
                    user_id: session.user_id.clone(),
                })
        });
        if subject.is_none() {
            return Err(metrics_error("machine_metrics_forbidden"));
        }
        require_machine_target(subject.as_ref(), &server_id)?;
        authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION,
            PolicyResource::new(RESOURCE_MACHINE, &server_id),
        )
        .await
        .map_err(|_| metrics_error("machine_metrics_forbidden"))?;
        let (_, actor) = control_audit_actor(session.as_deref(), subject.as_ref());
        let admission = Admission::take(&workspace_id, &server_id, actor.unwrap_or("machine"))?;
        let query = parse_query(raw.as_deref(), Utc::now())?;
        let data = tokio::time::timeout(
            Duration::from_secs(10),
            state.send_command(
                &workspace_id,
                &server_id,
                AgentCommand::MachineMetrics {
                    query: query.clone(),
                },
            ),
        )
        .await
        .map_err(|_| metrics_error("machine_metrics_timeout"))?
        .map_err(|error| match error.code.as_str() {
            "server_offline" => metrics_error("machine_metrics_offline"),
            "unsupported_command" | "unsupported_capability" | "controller_upgrade_required" => {
                metrics_error("machine_metrics_unsupported")
            }
            code if code.starts_with("machine_metrics_") => metrics_error(code),
            _ => metrics_error("machine_metrics_invalid_response"),
        })?;
        // Bound untrusted shapes before any second serialization; use typed
        // validation and re-encoding so unknown extension data never forwards.
        let response: MachineMetricsResponse = serde_json::from_value(data)
            .map_err(|_| metrics_error("machine_metrics_invalid_response"))?;
        response
            .validate(&query, &server_id, Utc::now())
            .map_err(|_| metrics_error("machine_metrics_invalid_response"))?;
        if let Some(session) = session.as_ref() {
            if !crate::api::events::revalidate_subscriber(&auth, session, &workspace_id)
                .await
                .is_ok_and(|valid| valid)
            {
                return Err(ApiFailure {
                    status: StatusCode::NOT_FOUND,
                    error: ProtocolError::new("workspace_not_found", "Workspace is unavailable"),
                });
            }
        } else if auth.authenticate_machine(&headers).await.is_err() {
            return Err(metrics_error("machine_metrics_forbidden"));
        }
        // Identity may disappear while the source query runs. Re-resolve it
        // before the final Policy check instead of publishing a stale grant.
        state.resolve_server(&workspace_id, &server_id).await?;
        authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION,
            PolicyResource::new(RESOURCE_MACHINE, &server_id),
        )
        .await
        .map_err(|_| metrics_error("machine_metrics_forbidden"))?;
        let bytes = serde_json::to_vec(&response)
            .map_err(|_| metrics_error("machine_metrics_invalid_response"))?;
        if bytes.len() > treer_protocol::MACHINE_METRICS_MAX_RESPONSE_BYTES {
            return Err(metrics_error("machine_metrics_invalid_response"));
        }
        // Bytes retains its owner through all clones, including transport
        // chunks after Body has been polled. A slow reader holds admission.
        let bytes = axum::body::Bytes::from_owner(AdmittedBytes {
            bytes,
            _admission: admission,
        });
        Ok::<_, ApiFailure>(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
    }
    .await;
    let mut response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_query_defaults_duplicates_and_bounds() {
        let now = Utc::now();
        assert_eq!(parse_query(None, now).unwrap().max_points, 360);
        for raw in [
            "from=bad",
            "max_points=1&max_points=2",
            "max_points=0",
            "max_points=721",
            "max_points=+1",
            "extra=1",
        ] {
            assert!(parse_query(Some(raw), now).is_err(), "{raw}");
        }
    }

    #[tokio::test]
    async fn authorized_dispatch_revalidation_unknown_fields_and_slow_reader() {
        use treer_protocol::*;
        let auth = AuthStore::for_test("test").await;
        auth.seed_test_workspace("default").await;
        sqlx::raw_sql("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('viewer','viewer@example.test','Viewer','fixture','now'); INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','viewer','member','now'); INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES('metrics-session','viewer','2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')").execute(&auth.pool()).await.unwrap();
        let session = CurrentSession {
            token: "metrics-session".into(),
            user_id: "viewer".into(),
            email: "viewer@example.test".into(),
            preferred_name: "Viewer".into(),
        };
        for case in ["allow", "denied", "invalid", "revoked"] {
            let state = crate::api::tests::state_with_managed_agent().await;
            let server = state.resolve_server("default", "machine-a").await.unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            state
                .register_server_instance_with_capabilities(
                    server,
                    Uuid::new_v4(),
                    "metrics-test".into(),
                    vec![CAPABILITY_MACHINE_METRICS.into()],
                    tx,
                )
                .await
                .unwrap();
            let policy = if case == "denied" {
                PolicyEngine::new(
                    crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
                        "policy_denied",
                        "fixture",
                    )),
                    vec![],
                )
            } else {
                PolicyEngine::allow_all()
            };
            let task = tokio::spawn({
                let state = state.clone();
                let auth = auth.clone();
                let session = session.clone();
                async move {
                    read_machine_metrics(
                        State(state),
                        Extension(auth),
                        Extension(policy),
                        Some(Extension(session)),
                        None,
                        HeaderMap::new(),
                        Path(("default".into(), "machine-a".into())),
                        RawQuery(None),
                    )
                    .await
                }
            });
            if case == "denied" {
                let response = task.await.unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
                assert!(rx.try_recv().is_err());
                continue;
            }
            let envelope = crate::api::tests::next_lifecycle_command(&mut rx).await;
            let AgentCommand::MachineMetrics { query } = envelope.command else {
                panic!("typed metrics dispatch")
            };
            let response = MachineMetricsResponse {
                schema: CAPABILITY_MACHINE_METRICS.into(),
                server_id: if case == "invalid" {
                    "other"
                } else {
                    "machine-a"
                }
                .into(),
                generated_at: Utc::now(),
                from: query.from,
                to: query.to,
                retained_from: None,
                truncated: false,
                sample_interval_ms: 5000,
                bucket_width_ms: 5000,
                stale_after_ms: 15000,
                logical_cpus: None,
                memory_method: MachineMemoryMethod::Unavailable,
                scope: "os_visible_machine".into(),
                gpus: vec![],
                inventory_truncated: false,
                gpu_status: MachineMetricStatus::WarmingUp,
                series: [
                    MachineMetricKind::CpuUtilization,
                    MachineMetricKind::RamUsed,
                    MachineMetricKind::RamAvailable,
                    MachineMetricKind::RamTotal,
                ]
                .into_iter()
                .map(|kind| MachineMetricSeries {
                    kind,
                    gpu_id: None,
                    unit: if kind == MachineMetricKind::CpuUtilization {
                        MachineMetricUnit::Percent
                    } else {
                        MachineMetricUnit::Bytes
                    },
                    current: MachineMetricCurrent {
                        status: MachineMetricStatus::WarmingUp,
                        value: None,
                        sampled_at: None,
                        age_ms: None,
                    },
                    points: vec![],
                })
                .collect(),
            };
            let mut data = serde_json::to_value(response).unwrap();
            data["future_extension"] = json!("must-not-forward");
            if case == "revoked" {
                auth.logout(&session.token).await.unwrap();
            }
            state
                .complete_command(CommandResult::success(envelope.command_id, data))
                .await;
            let response = task.await.unwrap();
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(
                response.status(),
                match case {
                    "invalid" => StatusCode::BAD_GATEWAY,
                    "revoked" => StatusCode::NOT_FOUND,
                    _ => StatusCode::OK,
                }
            );
            if case == "allow" {
                assert!(Admission::take("default", "machine-a", "viewer").is_err());
                let bytes =
                    axum::body::to_bytes(response.into_body(), MACHINE_METRICS_MAX_RESPONSE_BYTES)
                        .await
                        .unwrap();
                assert!(!std::str::from_utf8(&bytes)
                    .unwrap()
                    .contains("must-not-forward"));
                assert!(Admission::take("default", "machine-a", "viewer").is_err());
                drop(bytes);
                assert!(Admission::take("default", "machine-a", "viewer").is_ok());
            }
        }
    }
}
