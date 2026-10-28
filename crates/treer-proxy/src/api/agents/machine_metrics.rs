//! Machine hardware metrics (`machine.metrics.v1`). The Controller keeps the
//! history in memory; this route asks for one window and stores nothing.
//! Whoever can see the machine in the workspace snapshot can read its load:
//! members and Agents of the workspace, and a machine credential for itself.
use super::*;
use treer_protocol::{MACHINE_METRICS_DEFAULT_WINDOW_SECONDS, MACHINE_METRICS_MAX_WINDOW_SECONDS};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MachineMetricsQuery {
    #[serde(default)]
    window_seconds: Option<u32>,
}

pub(crate) async fn machine_metrics(
    State(state): State<AppState>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Query(query): Query<MachineMetricsQuery>,
) -> Result<Response, ApiFailure> {
    let window_seconds = query
        .window_seconds
        .unwrap_or(MACHINE_METRICS_DEFAULT_WINDOW_SECONDS);
    if !(1..=MACHINE_METRICS_MAX_WINDOW_SECONDS).contains(&window_seconds) {
        return Err(ApiFailure::bad_request(
            "invalid_request",
            "window_seconds must be between 1 and 86400",
        ));
    }
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject =
        control_policy_subject(&state, machine.as_deref(), &headers, &workspace_id).await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    // An old Controller lacks the capability: the dispatch refuses it.
    let data = state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::MachineMetrics { window_seconds },
        )
        .await
        .map_err(|error| {
            if error.code == "unsupported_command" {
                unsupported()
            } else {
                error.into()
            }
        })?;
    let mut response = Json(data).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn unsupported() -> ApiFailure {
    ApiFailure {
        status: StatusCode::NOT_IMPLEMENTED,
        error: ProtocolError::new(
            "machine_metrics_unsupported",
            "this machine's Controller does not report hardware metrics (machine.metrics.v1); update it",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use treer_protocol::{
        CommandResult, GpuStatus, MachineMetrics, ProxyMessage, CAPABILITY_MACHINE_METRICS,
    };

    /// `state_with_managed_agent` with machine-a reconnected advertising
    /// `capabilities`; a fake Controller answers metrics commands.
    async fn connected(capabilities: &[&str]) -> (AppState, tokio::task::JoinHandle<Vec<u32>>) {
        let state = crate::api::tests::state_with_managed_agent().await;
        let server = state.live_server("machine-a").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let connection = uuid::Uuid::new_v4();
        state
            .register_server_instance_with_capabilities(
                server,
                connection,
                "metrics-test".into(),
                capabilities.iter().map(|value| value.to_string()),
                tx,
            )
            .await
            .unwrap();
        let controller = state.clone();
        let task = tokio::spawn(async move {
            let mut windows = Vec::new();
            while let Some(frame) = rx.recv().await {
                let SocketFrame::Text(encoded) = frame else {
                    continue;
                };
                let Ok(ProxyMessage::Command { envelope }) = serde_json::from_str(&encoded) else {
                    continue;
                };
                let AgentCommand::MachineMetrics { window_seconds } = envelope.command else {
                    panic!("unexpected command {:?}", envelope.command);
                };
                windows.push(window_seconds);
                let metrics = MachineMetrics {
                    cpu_count: 8,
                    gpu_status: GpuStatus::Unsupported,
                    gpus: vec![],
                    sample_seconds: 5,
                    resolution_seconds: 5,
                    window_seconds,
                    samples: vec![],
                    latest: None,
                };
                controller
                    .complete_command_from(
                        "default",
                        "machine-a",
                        connection,
                        CommandResult::success(envelope.command_id, metrics),
                    )
                    .await
                    .unwrap();
                if windows.len() == 3 {
                    break;
                }
            }
            windows
        });
        (state, task)
    }

    async fn get(
        state: &AppState,
        machine: Option<Extension<MachineSession>>,
        headers: HeaderMap,
        server_id: &str,
        window_seconds: Option<u32>,
    ) -> Result<Response, ApiFailure> {
        machine_metrics(
            State(state.clone()),
            machine,
            headers,
            Path(("default".into(), server_id.into())),
            Query(MachineMetricsQuery { window_seconds }),
        )
        .await
    }

    fn machine(server_id: &str) -> Option<Extension<MachineSession>> {
        Some(Extension(MachineSession {
            server_id: Some(server_id.into()),
            workspace_id: Some("default".into()),
        }))
    }

    #[tokio::test]
    async fn members_agents_and_the_machine_itself_read_the_controller_window() {
        let (state, controller) = connected(&[CAPABILITY_MACHINE_METRICS]).await;
        // A browser member (membership is enforced by the workspace middleware).
        let response = get(&state, None, HeaderMap::new(), "machine-a", None)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let metrics: MachineMetrics = serde_json::from_slice(&body).unwrap();
        assert_eq!(metrics.cpu_count, 8);
        // An Agent of the workspace.
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
        get(
            &state,
            machine("machine-a"),
            headers,
            "machine-a",
            Some(900),
        )
        .await
        .unwrap();
        // The machine credential (local operator CLI) for its own machine.
        get(
            &state,
            machine("machine-a"),
            HeaderMap::new(),
            "machine-a",
            Some(30),
        )
        .await
        .unwrap();
        assert_eq!(
            controller.await.unwrap(),
            vec![MACHINE_METRICS_DEFAULT_WINDOW_SECONDS, 900, 30]
        );
    }

    #[tokio::test]
    async fn old_controllers_wrong_machines_and_bad_windows_are_refused_before_dispatch() {
        let (state, _controller) = connected(&[]).await;
        let error = get(&state, None, HeaderMap::new(), "machine-a", None)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(error.error.code, "machine_metrics_unsupported");
        let error = get(
            &state,
            machine("machine-b"),
            HeaderMap::new(),
            "machine-a",
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.error.code, "agent_identity_required");
        for window in [0, MACHINE_METRICS_MAX_WINDOW_SECONDS + 1] {
            let error = get(&state, None, HeaderMap::new(), "machine-a", Some(window))
                .await
                .unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
        }
        let error = get(&state, None, HeaderMap::new(), "missing", None)
            .await
            .unwrap_err();
        assert_eq!(error.error.code, "server_not_found");
    }
}
