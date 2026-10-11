use std::collections::HashMap;

use axum::extract::ws::{Message as BrowserMessage, WebSocket};
use axum::extract::{DefaultBodyLimit, Path, Query, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as ProxyMessage;
use treer_protocol::{
    AcknowledgeMessagesRequest, ApiError, BuildInfo, CreateAgentLaunchProfileRequest,
    CreateAgentRequest, CreateAppDeploymentRequest, CreateMachineServiceRequest,
    CreateServiceIngressRequest, CreateVirtualNetworkHostRequest, ImportMessagesRequest,
    InputAgentRequest, LaunchAgentProfileRequest, ListMessagesQuery, MachineExecRequest,
    PolicyProviderInvalidationRequest, PromptAgentRequest, ProtocolError, ReceiveMessagesRequest,
    RegisterAgentInterfaceRequest, RenameRequest, SendMessageRequest, SetAgentStartupRequest,
    TerminalServerMessage, UpdateAgentLaunchProfileRequest, UpdateMachineServiceRequest,
    UpdateServiceIngressRequest, UploadMachineFileRequest, WorkloadIdentityTokenRequest,
    AGENT_ID_HEADER, OPERATOR_CREDENTIAL_HEADER, WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;
use uuid::Uuid;

use crate::controller::ControllerRuntime;

#[path = "local_api_handlers.rs"]
mod handlers;
use handlers::*;

const MAX_MACHINE_UPLOAD_BODY_BYTES: usize = 23 * 1024 * 1024;

async fn publish_public_tcp(
    State(state): State<LocalApiState>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!(
                    "services/{}/public-tcp",
                    percent_encoding::utf8_percent_encode(
                        &service_id,
                        percent_encoding::NON_ALPHANUMERIC
                    )
                ),
                &request,
                agent.as_ref(),
            )
            .await?,
    ))
}

async fn unpublish_public_tcp(
    State(state): State<LocalApiState>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(
                &format!(
                    "services/{}/public-tcp",
                    percent_encoding::utf8_percent_encode(
                        &service_id,
                        percent_encoding::NON_ALPHANUMERIC
                    )
                ),
                agent.as_ref(),
            )
            .await?,
    ))
}

async fn service_connect(
    State(state): State<LocalApiState>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    mut request: axum::extract::Request,
) -> Result<Response, LocalApiError> {
    let agent = validated_source_agent(&state, &headers)?;
    let upgrade = headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("treer-tcp") {
        return Err(LocalApiError::bad_gateway(
            "Upgrade: treer-tcp is required".into(),
        ));
    }
    let mut upstream = state
        .proxy_websocket_url(&format!(
            "services/{}/connect",
            percent_encoding::utf8_percent_encode(&service_id, percent_encoding::NON_ALPHANUMERIC)
        ))?
        .as_str()
        .into_client_request()
        .map_err(|e| LocalApiError::bad_gateway(e.to_string()))?;
    if let Some(token) = &state.machine_token {
        upstream.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| LocalApiError::bad_gateway(e.to_string()))?,
        );
    }
    if let Some(agent) = agent {
        for (key, value) in [
            (AGENT_ID_HEADER, agent.agent_id),
            (WORKLOAD_CREDENTIAL_HEADER, agent.workload_credential),
        ] {
            upstream.headers_mut().insert(
                key,
                HeaderValue::from_str(&value)
                    .map_err(|e| LocalApiError::bad_gateway(e.to_string()))?,
            );
        }
    }
    let (remote, _) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio_tungstenite::connect_async(upstream),
    )
    .await
    .map_err(|_| LocalApiError::bad_gateway("service connection timed out".into()))?
    .map_err(|error| match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => LocalApiError {
            status: response.status(),
            error: ProtocolError::new(
                "service_connection_denied",
                "service connection denied or unavailable",
            ),
        },
        error => LocalApiError::bad_gateway(error.to_string()),
    })?;
    let local = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        if let Ok(local) = local.await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut reader, mut writer) = tokio::io::split(hyper_util::rt::TokioIo::new(local));
            let (mut outgoing, mut incoming) = remote.split();
            let send = async {
                let mut buffer = vec![0; 32 * 1024];
                loop {
                    let count = reader.read(&mut buffer).await.map_err(|_| ())?;
                    if count == 0 {
                        outgoing
                            .send(ProxyMessage::Text(
                                treer_protocol::SERVICE_STREAM_EOF.into(),
                            ))
                            .await
                            .map_err(|_| ())?;
                        return Ok::<(), ()>(());
                    }
                    outgoing
                        .send(ProxyMessage::Binary(buffer[..count].to_vec().into()))
                        .await
                        .map_err(|_| ())?;
                }
            };
            let receive = async {
                while let Some(message) = incoming.next().await {
                    match message.map_err(|_| ())? {
                        ProxyMessage::Binary(bytes) => {
                            writer.write_all(&bytes).await.map_err(|_| ())?
                        }
                        ProxyMessage::Text(text) if text == treer_protocol::SERVICE_STREAM_EOF => {
                            writer.shutdown().await.map_err(|_| ())?;
                            return Ok(());
                        }
                        ProxyMessage::Ping(_) | ProxyMessage::Pong(_) => {}
                        _ => return Err(()),
                    }
                }
                Err(())
            };
            let _ = tokio::try_join!(send, receive);
        }
    });
    Ok((
        StatusCode::SWITCHING_PROTOCOLS,
        [("connection", "upgrade"), ("upgrade", "treer-tcp")],
    )
        .into_response())
}

async fn cli_session(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, LocalApiError> {
    let agent = validated_source_agent(&state, &headers)?;
    let identity = match agent {
        Some(agent) => vec![
            ("TREER_AGENT_ID".into(), agent.agent_id),
            ("TREER_SERVER_ID".into(), state.server_id.clone()),
            (
                "TREER_WORKLOAD_CREDENTIAL".into(),
                agent.workload_credential,
            ),
        ],
        None => vec![(
            "TREER_OPERATOR_CREDENTIAL".into(),
            headers
                .get(OPERATOR_CREDENTIAL_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
        )],
    };
    let base = state.runtime.agent_server_url().to_owned();
    Ok(ws.on_upgrade(move |socket| {
        crate::cli_session::serve(socket, base, state.workspace_id, identity)
    }))
}

#[derive(Clone)]
pub struct LocalApiState {
    client: reqwest::Client,
    proxy_http: Url,
    workspace_id: String,
    server_id: String,
    controller_epoch: String,
    host_build: BuildInfo,
    machine_token: Option<String>,
    operator_credential: Option<String>,
    runtime: ControllerRuntime,
}

impl LocalApiState {
    pub fn new(
        proxy_http: Url,
        workspace_id: String,
        server_id: String,
        machine_token: Option<String>,
        operator_credential: Option<String>,
        host_build: BuildInfo,
        runtime: ControllerRuntime,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            proxy_http,
            workspace_id,
            server_id,
            controller_epoch: Uuid::new_v4().to_string(),
            host_build,
            machine_token,
            operator_credential,
            runtime,
        }
    }

    fn proxy_url(&self, suffix: &str) -> Result<Url, LocalApiError> {
        self.proxy_http
            .join(&format!(
                "/agent/workspaces/{}/{}",
                self.workspace_id, suffix
            ))
            .map_err(|err| LocalApiError::bad_gateway(err.to_string()))
    }

    fn proxy_websocket_url(&self, suffix: &str) -> Result<Url, LocalApiError> {
        let mut url = self.proxy_url(suffix)?;
        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            scheme => {
                return Err(LocalApiError::bad_gateway(format!(
                    "unsupported proxy URL scheme {scheme}"
                )))
            }
        };
        url.set_scheme(scheme)
            .map_err(|_| LocalApiError::bad_gateway("invalid proxy URL scheme".to_string()))?;
        Ok(url)
    }

    async fn request(
        &self,
        method: reqwest::Method,
        suffix: &str,
        body: Option<&Value>,
        source_agent: Option<&ValidatedAgent>,
    ) -> Result<Value, LocalApiError> {
        let mut request = self.client.request(method, self.proxy_url(suffix)?);
        if let Some(token) = &self.machine_token {
            request = request.bearer_auth(token);
        }
        if let Some(agent) = source_agent {
            request = request
                .header(AGENT_ID_HEADER, &agent.agent_id)
                .header(WORKLOAD_CREDENTIAL_HEADER, &agent.workload_credential);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|err| LocalApiError::bad_gateway(err.to_string()))?;
        decode(response).await
    }

    async fn get_as(
        &self,
        suffix: &str,
        source_agent: Option<&ValidatedAgent>,
    ) -> Result<Value, LocalApiError> {
        self.request(reqwest::Method::GET, suffix, None, source_agent)
            .await
    }

    async fn post_as(
        &self,
        suffix: &str,
        body: &Value,
        source_agent: Option<&ValidatedAgent>,
    ) -> Result<Value, LocalApiError> {
        self.request(reqwest::Method::POST, suffix, Some(body), source_agent)
            .await
    }

    async fn patch_as(
        &self,
        suffix: &str,
        body: &Value,
        source_agent: Option<&ValidatedAgent>,
    ) -> Result<Value, LocalApiError> {
        self.request(reqwest::Method::PATCH, suffix, Some(body), source_agent)
            .await
    }

    async fn put_as(
        &self,
        suffix: &str,
        body: &Value,
        source_agent: Option<&ValidatedAgent>,
    ) -> Result<Value, LocalApiError> {
        self.request(reqwest::Method::PUT, suffix, Some(body), source_agent)
            .await
    }

    async fn delete_as(
        &self,
        suffix: &str,
        source_agent: Option<&ValidatedAgent>,
    ) -> Result<Value, LocalApiError> {
        self.request(reqwest::Method::DELETE, suffix, None, source_agent)
            .await
    }
}

pub fn router(state: LocalApiState) -> Router {
    Router::new()
        .route("/api/cli/session", get(cli_session))
        .route("/api/services/{service_id}/connect", post(service_connect))
        .route(
            "/api/services/{service_id}/public-tcp",
            post(publish_public_tcp).delete(unpublish_public_tcp),
        )
        .route("/api/health", get(health))
        .route("/api/discovery", get(discovery))
        .route("/api/identity/token", post(issue_identity_token))
        .route(
            "/api/policy-provider/invalidate",
            post(invalidate_policy_provider),
        )
        .route("/api/humans", get(list_humans))
        .route(
            "/api/machines/{server_id}",
            axum::routing::patch(rename_machine).delete(delete_machine),
        )
        .route("/api/machines/{server_id}/exec", post(exec_machine))
        .route(
            "/api/machines/{server_id}/files",
            post(upload_machine_file).layer(DefaultBodyLimit::max(MAX_MACHINE_UPLOAD_BODY_BYTES)),
        )
        .route("/api/local/agents", get(list_local_agents))
        .route("/api/agents", get(list_agents).post(create_agent))
        .route(
            "/api/apps",
            get(list_app_deployments).post(create_app_deployment),
        )
        .route(
            "/api/apps/{app_id}",
            get(get_app_deployment).delete(delete_app_deployment),
        )
        .route("/api/apps/{app_id}/start", post(start_app_deployment))
        .route("/api/apps/{app_id}/stop", post(stop_app_deployment))
        .route("/api/apps/{app_id}/restart", post(restart_app_deployment))
        .route(
            "/api/launch-profiles",
            get(list_agent_launch_profiles).post(create_agent_launch_profile),
        )
        .route(
            "/api/launch-profiles/{profile_id}",
            get(get_agent_launch_profile)
                .patch(update_agent_launch_profile)
                .delete(delete_agent_launch_profile),
        )
        .route(
            "/api/launch-profiles/{profile_id}/launch",
            post(launch_agent_profile),
        )
        .route(
            "/api/services",
            get(list_machine_services).post(create_machine_service),
        )
        .route(
            "/api/services/{service_id}",
            axum::routing::patch(update_machine_service).delete(delete_machine_service),
        )
        .route(
            "/api/services/{service_id}/probe",
            post(probe_machine_service),
        )
        .route(
            "/api/interface",
            get(get_agent_interface)
                .put(register_agent_interface)
                .delete(clear_agent_interface),
        )
        .route(
            "/api/agent/startup",
            get(get_agent_startup)
                .put(set_agent_startup)
                .delete(clear_agent_startup),
        )
        .route("/api/runtime/restore", post(restore_runtime))
        .route(
            "/api/virtual-hosts",
            get(list_virtual_network_hosts).post(create_virtual_network_host),
        )
        .route(
            "/api/virtual-hosts/{hostname}",
            axum::routing::delete(delete_virtual_network_host),
        )
        .route(
            "/api/publish",
            get(list_service_ingresses).post(create_service_ingress),
        )
        .route(
            "/api/publish/{ingress_id}",
            axum::routing::patch(update_service_ingress).delete(delete_service_ingress),
        )
        .route(
            "/api/agents/{agent_id}",
            get(get_agent).patch(rename_agent).delete(delete_agent),
        )
        .route("/api/agents/{agent_id}/terminal", get(agent_terminal))
        .route("/api/agents/{agent_id}/prompt", post(prompt_agent))
        .route("/api/agents/{agent_id}/input", post(input_agent))
        .route("/api/agents/{agent_id}/output", get(read_agent))
        .route(
            "/api/agents/{agent_id}/prompt-queue",
            get(read_agent_prompt_queue),
        )
        .route(
            "/api/agents/{agent_id}/transcript",
            get(read_agent_transcript),
        )
        .route("/api/agents/{agent_id}/stop", post(stop_agent))
        .route(
            "/api/messages",
            get(list_core_messages).post(send_core_message),
        )
        .route("/api/messages/receive", post(receive_core_messages))
        .route("/api/messages/ack", post(acknowledge_core_messages))
        .route("/api/messages/import", post(import_core_messages))
        .route("/api/messages/{message_id}", get(get_core_message))
        .with_state(state)
}

async fn decode(response: reqwest::Response) -> Result<Value, LocalApiError> {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let value = response
        .json::<Value>()
        .await
        .map_err(|err| LocalApiError::bad_gateway(err.to_string()))?;
    if status.is_success() {
        Ok(value)
    } else {
        let error = value
            .get("error")
            .and_then(|value| serde_json::from_value::<ProtocolError>(value.clone()).ok())
            .unwrap_or_else(|| ProtocolError::new("proxy_error", value.to_string()));
        Err(LocalApiError { status, error })
    }
}

#[derive(Debug)]
pub struct LocalApiError {
    status: StatusCode,
    error: ProtocolError,
}

impl LocalApiError {
    fn bad_request_protocol(error: ProtocolError) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error,
        }
    }
    fn bad_gateway(message: String) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            error: ProtocolError::new("proxy_unavailable", message),
        }
    }

    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: ProtocolError::new("invalid_request", message),
        }
    }

    fn unauthorized(error: ProtocolError) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error,
        }
    }
}

impl IntoResponse for LocalApiError {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.error })).into_response()
    }
}

#[cfg(test)]
#[path = "local_api_tests.rs"]
mod tests;
