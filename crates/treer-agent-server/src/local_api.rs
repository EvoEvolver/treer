use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use treer_protocol::{
    ApiError, CreateAgentRequest, InputAgentRequest, PromptAgentRequest, ProtocolError,
    RenameRequest,
};
use url::Url;

#[derive(Clone)]
pub struct LocalApiState {
    client: reqwest::Client,
    proxy_http: Url,
    workspace_id: String,
    machine_token: Option<String>,
}

impl LocalApiState {
    pub fn new(proxy_http: Url, workspace_id: String, machine_token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            proxy_http,
            workspace_id,
            machine_token,
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

    async fn get(&self, suffix: &str) -> Result<Value, LocalApiError> {
        let mut request = self.client.get(self.proxy_url(suffix)?);
        if let Some(token) = &self.machine_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|err| LocalApiError::bad_gateway(err.to_string()))?;
        decode(response).await
    }

    async fn post(&self, suffix: &str, body: &Value) -> Result<Value, LocalApiError> {
        let mut request = self.client.post(self.proxy_url(suffix)?).json(body);
        if let Some(token) = &self.machine_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|err| LocalApiError::bad_gateway(err.to_string()))?;
        decode(response).await
    }

    async fn patch(&self, suffix: &str, body: &Value) -> Result<Value, LocalApiError> {
        let mut request = self.client.patch(self.proxy_url(suffix)?).json(body);
        if let Some(token) = &self.machine_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|err| LocalApiError::bad_gateway(err.to_string()))?;
        decode(response).await
    }
}

pub fn router(state: LocalApiState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/discovery", get(discovery))
        .route(
            "/api/machines/{server_id}",
            axum::routing::patch(rename_machine),
        )
        .route("/api/agents", get(list_agents).post(create_agent))
        .route("/api/agents/{agent_id}", get(get_agent).patch(rename_agent))
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
    }))
}

async fn discovery(State(state): State<LocalApiState>) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(state.get("snapshot").await?))
}

async fn list_agents(State(state): State<LocalApiState>) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(state.get("agents").await?))
}

async fn get_agent(
    State(state): State<LocalApiState>,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(state.get(&format!("agents/{agent_id}")).await?))
}

async fn create_agent(
    State(state): State<LocalApiState>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(
        state
            .post(
                "agents",
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
            )
            .await?,
    ))
}

async fn rename_machine(
    State(state): State<LocalApiState>,
    Path(server_id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(
        state
            .patch(
                &format!("servers/{server_id}"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
            )
            .await?,
    ))
}

async fn rename_agent(
    State(state): State<LocalApiState>,
    Path(agent_id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(
        state
            .patch(
                &format!("agents/{agent_id}"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
            )
            .await?,
    ))
}

async fn prompt_agent(
    State(state): State<LocalApiState>,
    Path(agent_id): Path<String>,
    Json(request): Json<PromptAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(
        state
            .post(
                &format!("agents/{agent_id}/prompt"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
            )
            .await?,
    ))
}

async fn input_agent(
    State(state): State<LocalApiState>,
    Path(agent_id): Path<String>,
    Json(request): Json<InputAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(
        state
            .post(
                &format!("agents/{agent_id}/input"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
            )
            .await?,
    ))
}

async fn read_agent(
    State(state): State<LocalApiState>,
    Path(agent_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, LocalApiError> {
    let suffix = query.get("lines").map_or_else(
        || format!("agents/{agent_id}/output"),
        |lines| format!("agents/{agent_id}/output?lines={lines}"),
    );
    Ok(Json(state.get(&suffix).await?))
}

async fn stop_agent(
    State(state): State<LocalApiState>,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    Ok(Json(
        state
            .post(&format!("agents/{agent_id}/stop"), &json!({}))
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
}

impl IntoResponse for LocalApiError {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.error })).into_response()
    }
}
