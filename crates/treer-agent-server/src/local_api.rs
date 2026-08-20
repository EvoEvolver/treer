use std::collections::HashMap;

use axum::extract::ws::{Message as BrowserMessage, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
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
    AgentInboxRequest, ApiError, BuildInfo, CreateAgentLaunchProfileRequest, CreateAgentRequest,
    CreateMachineServiceRequest, CreateServiceIngressRequest, CreateVirtualNetworkHostRequest,
    InputAgentRequest, LaunchAgentProfileRequest, PromptAgentRequest, ProtocolError, RenameRequest,
    SendAgentMailRequest, TerminalServerMessage, UpdateAgentLaunchProfileRequest,
    UpdateMachineServiceRequest, UpdateServiceIngressRequest, WorkloadIdentityTokenRequest,
    AGENT_ID_HEADER, OPERATOR_CREDENTIAL_HEADER, WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;
use uuid::Uuid;

use crate::controller::ControllerRuntime;

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
        .route("/api/health", get(health))
        .route("/api/discovery", get(discovery))
        .route("/api/identity/token", post(issue_identity_token))
        .route("/api/mail", post(send_mail))
        .route("/api/inbox", post(read_inbox))
        .route("/api/humans", get(list_humans))
        .route(
            "/api/machines/{server_id}",
            axum::routing::patch(rename_machine).delete(delete_machine),
        )
        .route("/api/local/agents", get(list_local_agents))
        .route("/api/agents", get(list_agents).post(create_agent))
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
        .route("/api/agents/{agent_id}/stop", post(stop_agent))
        .with_state(state)
}

async fn health(State(state): State<LocalApiState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "treer-agent-server",
        "workspace_id": state.workspace_id,
        "server_id": state.server_id,
        "controller_epoch": state.controller_epoch,
        "controller_build": BuildInfo {
            version: treer_build_info::VERSION.to_string(),
            git_commit: treer_build_info::GIT_COMMIT.to_string(),
        },
        "host_build": state.host_build,
    }))
}

async fn discovery(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("snapshot", source_agent.as_ref()).await?))
}

async fn issue_identity_token(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<WorkloadIdentityTokenRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(
        state
            .post_as("identity/token", &body, Some(&agent_id))
            .await?,
    ))
}

async fn send_mail(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<SendAgentMailRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(state.post_as("mail", &body, Some(&agent_id)).await?))
}

async fn read_inbox(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<AgentInboxRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(state.post_as("inbox", &body, Some(&agent_id)).await?))
}

async fn list_humans(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("humans", Some(&agent_id)).await?))
}

async fn list_agents(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("agents", source_agent.as_ref()).await?))
}

async fn list_local_agents(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    authenticate_operator(&state, &headers)?;
    Ok(Json(json!({ "agents": state.runtime.list() })))
}

async fn list_machine_services(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("services", Some(&agent_id)).await?))
}

async fn create_machine_service(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateMachineServiceRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "services",
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                Some(&agent_id),
            )
            .await?,
    ))
}

async fn update_machine_service(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(service_id): Path<String>,
    Json(request): Json<UpdateMachineServiceRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("services/{service_id}"),
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                Some(&agent_id),
            )
            .await?,
    ))
}

async fn delete_machine_service(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(service_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("services/{service_id}"), Some(&agent_id))
            .await?,
    ))
}

async fn probe_machine_service(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(service_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("services/{service_id}/probe"),
                &json!({}),
                Some(&agent_id),
            )
            .await?,
    ))
}

async fn list_virtual_network_hosts(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("virtual-hosts", Some(&agent_id)).await?))
}

async fn create_virtual_network_host(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateVirtualNetworkHostRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "virtual-hosts",
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                Some(&agent_id),
            )
            .await?,
    ))
}

async fn delete_virtual_network_host(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(hostname): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("virtual-hosts/{hostname}"), Some(&agent_id))
            .await?,
    ))
}

async fn list_service_ingresses(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("ingresses", Some(&agent)).await?))
}

async fn create_service_ingress(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateServiceIngressRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "ingresses",
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                Some(&agent),
            )
            .await?,
    ))
}

async fn update_service_ingress(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(ingress_id): Path<String>,
    Json(request): Json<UpdateServiceIngressRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("ingresses/{ingress_id}"),
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                Some(&agent),
            )
            .await?,
    ))
}

async fn delete_service_ingress(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(ingress_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("ingresses/{ingress_id}"), Some(&agent))
            .await?,
    ))
}

fn source_agent_id(headers: &HeaderMap) -> Result<&str, LocalApiError> {
    let agent_id = headers
        .get(AGENT_ID_HEADER)
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .map_err(|_| LocalApiError::bad_request("invalid agent identity".to_string()))
        })
        .transpose()
        .map(|value| value.filter(|value| !value.is_empty()))?;
    agent_id
        .ok_or_else(|| LocalApiError::bad_request("managed agent identity is required".to_string()))
}

fn workload_identity(headers: &HeaderMap) -> Result<(&str, &str), LocalApiError> {
    let agent_id = source_agent_id(headers)?;
    let credential = headers
        .get(WORKLOAD_CREDENTIAL_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            LocalApiError::unauthorized(ProtocolError::new(
                "workload_credential_required",
                "managed agent workload credential is required",
            ))
        })?;
    Ok((agent_id, credential))
}

fn optional_workload_identity(headers: &HeaderMap) -> Result<Option<(&str, &str)>, LocalApiError> {
    let has_agent_id = headers.contains_key(AGENT_ID_HEADER);
    let has_credential = headers.contains_key(WORKLOAD_CREDENTIAL_HEADER);
    match (has_agent_id, has_credential) {
        (false, false) => Ok(None),
        (true, true) => workload_identity(headers).map(Some),
        (true, false) => Err(LocalApiError::unauthorized(ProtocolError::new(
            "workload_credential_required",
            "managed agent workload credential is required",
        ))),
        (false, true) => Err(LocalApiError::unauthorized(ProtocolError::new(
            "agent_identity_required",
            "managed agent identity is required with a workload credential",
        ))),
    }
}

#[derive(Clone, Debug)]
struct ValidatedAgent {
    agent_id: String,
    workload_credential: String,
}

fn authenticate_operator(state: &LocalApiState, headers: &HeaderMap) -> Result<(), LocalApiError> {
    let expected = state
        .operator_credential
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(operator_auth_required)?;
    if !operator_credential_matches(expected, headers) {
        return Err(operator_auth_required());
    }
    Ok(())
}

fn operator_credential_matches(expected: &str, headers: &HeaderMap) -> bool {
    let supplied = headers
        .get(OPERATOR_CREDENTIAL_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    !expected.is_empty()
        && expected.len() == supplied.len()
        && expected.as_bytes().ct_eq(supplied.as_bytes()).unwrap_u8() == 1
}

fn operator_auth_required() -> LocalApiError {
    LocalApiError::unauthorized(ProtocolError::new(
        "operator_authentication_required",
        "local operator credential is required",
    ))
}

fn validated_source_agent(
    state: &LocalApiState,
    headers: &HeaderMap,
) -> Result<Option<ValidatedAgent>, LocalApiError> {
    let Some((agent_id, workload_credential)) = optional_workload_identity(headers)? else {
        authenticate_operator(state, headers)?;
        return Ok(None);
    };
    state
        .runtime
        .authenticate_agent(agent_id, workload_credential)
        .map_err(LocalApiError::unauthorized)?;
    Ok(Some(ValidatedAgent {
        agent_id: agent_id.to_string(),
        workload_credential: workload_credential.to_string(),
    }))
}

fn required_validated_source_agent(
    state: &LocalApiState,
    headers: &HeaderMap,
) -> Result<ValidatedAgent, LocalApiError> {
    validated_source_agent(state, headers)?.ok_or_else(|| {
        LocalApiError::unauthorized(ProtocolError::new(
            "workload_identity_required",
            "managed agent identity and workload credential are required",
        ))
    })
}

async fn get_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as(&format!("agents/{agent_id}"), source_agent.as_ref())
            .await?,
    ))
}

async fn create_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "agents",
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn list_agent_launch_profiles(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as("launch-profiles", source_agent.as_ref())
            .await?,
    ))
}

async fn get_agent_launch_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as(
                &format!("launch-profiles/{profile_id}"),
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn create_agent_launch_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentLaunchProfileRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "launch-profiles",
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn update_agent_launch_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
    Json(request): Json<UpdateAgentLaunchProfileRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("launch-profiles/{profile_id}"),
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn delete_agent_launch_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(
                &format!("launch-profiles/{profile_id}"),
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn launch_agent_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
    Json(request): Json<LaunchAgentProfileRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("launch-profiles/{profile_id}/launch"),
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn rename_machine(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("servers/{server_id}"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn delete_machine(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("servers/{server_id}"), source_agent.as_ref())
            .await?,
    ))
}

async fn rename_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("agents/{agent_id}"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn delete_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("agents/{agent_id}"), source_agent.as_ref())
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
struct TerminalQuery {
    #[serde(default = "default_terminal_cols")]
    cols: u16,
    #[serde(default = "default_terminal_rows")]
    rows: u16,
}

const fn default_terminal_cols() -> u16 {
    120
}

const fn default_terminal_rows() -> u16 {
    36
}

async fn agent_terminal(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Query(query): Query<TerminalQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    let mut upstream = state.proxy_websocket_url(&format!("agents/{agent_id}/terminal"))?;
    upstream
        .query_pairs_mut()
        .append_pair("cols", &query.cols.max(1).to_string())
        .append_pair("rows", &query.rows.max(1).to_string());
    Ok(ws.on_upgrade(move |socket| relay_terminal(socket, state, upstream, source_agent)))
}

async fn relay_terminal(
    socket: WebSocket,
    state: LocalApiState,
    upstream: Url,
    source_agent: Option<ValidatedAgent>,
) {
    if let Err(error) = relay_terminal_inner(socket, state, upstream, source_agent).await {
        tracing::warn!(error = %error.message, "local terminal relay closed");
    }
}

async fn relay_terminal_inner(
    mut socket: WebSocket,
    state: LocalApiState,
    upstream: Url,
    source_agent: Option<ValidatedAgent>,
) -> Result<(), ProtocolError> {
    let mut request = upstream
        .as_str()
        .into_client_request()
        .map_err(|error| ProtocolError::new("proxy_unavailable", error.to_string()))?;
    if let Some(token) = &state.machine_token {
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|error| ProtocolError::new("invalid_machine_token", error.to_string()))?,
        );
    }
    if let Some(agent) = source_agent {
        request.headers_mut().insert(
            AGENT_ID_HEADER,
            HeaderValue::from_str(&agent.agent_id)
                .map_err(|error| ProtocolError::new("invalid_agent_identity", error.to_string()))?,
        );
        request.headers_mut().insert(
            WORKLOAD_CREDENTIAL_HEADER,
            HeaderValue::from_str(&agent.workload_credential).map_err(|error| {
                ProtocolError::new("invalid_workload_credential", error.to_string())
            })?,
        );
    }
    let upstream = match tokio_tungstenite::connect_async(request).await {
        Ok((upstream, _)) => upstream,
        Err(error) => {
            send_terminal_error(&mut socket, "proxy_unavailable", error.to_string()).await;
            return Err(ProtocolError::new("proxy_unavailable", error.to_string()));
        }
    };
    let (mut browser_out, mut browser_in) = socket.split();
    let (mut proxy_out, mut proxy_in) = upstream.split();

    loop {
        tokio::select! {
            message = browser_in.next() => {
                let Some(Ok(message)) = message else { break };
                let message = match message {
                    BrowserMessage::Text(text) => ProxyMessage::Text(text.to_string().into()),
                    BrowserMessage::Binary(data) => ProxyMessage::Binary(data),
                    BrowserMessage::Ping(data) => ProxyMessage::Ping(data),
                    BrowserMessage::Pong(data) => ProxyMessage::Pong(data),
                    BrowserMessage::Close(_) => break,
                };
                if proxy_out.send(message).await.is_err() {
                    break;
                }
            }
            message = proxy_in.next() => {
                let Some(Ok(message)) = message else { break };
                let message = match message {
                    ProxyMessage::Text(text) => BrowserMessage::Text(text.to_string().into()),
                    ProxyMessage::Binary(data) => BrowserMessage::Binary(data),
                    ProxyMessage::Ping(data) => BrowserMessage::Ping(data),
                    ProxyMessage::Pong(data) => BrowserMessage::Pong(data),
                    ProxyMessage::Close(_) => break,
                    ProxyMessage::Frame(_) => continue,
                };
                if browser_out.send(message).await.is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn send_terminal_error(socket: &mut WebSocket, code: &str, message: String) {
    let message = TerminalServerMessage::Error {
        error: ProtocolError::new(code, message),
    };
    if let Ok(encoded) = serde_json::to_string(&message) {
        let _ = socket.send(BrowserMessage::Text(encoded.into())).await;
    }
}

async fn prompt_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(request): Json<PromptAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("agents/{agent_id}/prompt"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn input_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(request): Json<InputAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("agents/{agent_id}/input"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

async fn read_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    let suffix = query.get("lines").map_or_else(
        || format!("agents/{agent_id}/output"),
        |lines| format!("agents/{agent_id}/output?lines={lines}"),
    );
    Ok(Json(state.get_as(&suffix, source_agent.as_ref()).await?))
}

async fn stop_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("agents/{agent_id}/stop"),
                &json!({}),
                source_agent.as_ref(),
            )
            .await?,
    ))
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
mod tests {
    use super::*;

    #[test]
    fn source_agent_identity_is_required_and_validated() {
        let mut headers = HeaderMap::new();
        assert!(source_agent_id(&headers).is_err());
        headers.insert(AGENT_ID_HEADER, " agent-a ".parse().expect("agent header"));
        assert_eq!(
            source_agent_id(&headers).unwrap_or_else(|_| panic!("valid identity")),
            "agent-a"
        );
    }

    #[test]
    fn workload_identity_requires_both_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
        assert!(workload_identity(&headers).is_err());
        headers.insert(
            WORKLOAD_CREDENTIAL_HEADER,
            "wlc_secret".parse().expect("credential header"),
        );
        assert_eq!(
            workload_identity(&headers).unwrap_or_else(|_| panic!("workload identity")),
            ("agent-a", "wlc_secret")
        );
    }

    #[test]
    fn optional_workload_identity_distinguishes_operator_and_agent_requests() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            optional_workload_identity(&headers).expect("operator request"),
            None
        );

        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
        let missing_credential =
            optional_workload_identity(&headers).expect_err("partial Agent identity");
        assert_eq!(
            missing_credential.error.code,
            "workload_credential_required"
        );

        headers.insert(
            WORKLOAD_CREDENTIAL_HEADER,
            "wlc_secret".parse().expect("credential header"),
        );
        assert_eq!(
            optional_workload_identity(&headers).expect("managed Agent request"),
            Some(("agent-a", "wlc_secret"))
        );

        headers.remove(AGENT_ID_HEADER);
        let missing_agent =
            optional_workload_identity(&headers).expect_err("credential without Agent ID");
        assert_eq!(missing_agent.error.code, "agent_identity_required");
    }

    #[test]
    fn operator_requests_require_the_private_controller_credential() {
        let mut headers = HeaderMap::new();
        assert!(!operator_credential_matches("opc_secret", &headers));
        headers.insert(
            OPERATOR_CREDENTIAL_HEADER,
            "opc_wrong".parse().expect("operator header"),
        );
        assert!(!operator_credential_matches("opc_secret", &headers));
        headers.insert(
            OPERATOR_CREDENTIAL_HEADER,
            "opc_secret".parse().expect("operator header"),
        );
        assert!(operator_credential_matches("opc_secret", &headers));
    }
}
