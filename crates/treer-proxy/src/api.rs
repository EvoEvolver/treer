use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::DefaultBodyLimit;
use axum::extract::{Extension, Form, OriginalUri, Path, Query, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, Version};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
#[cfg(test)]
use treer_protocol::ApiError;
use treer_protocol::{
    installer_base_prompt, installer_composer_ready, pick_existing_installer_agent,
    recipe_installer_kind_allowed, recipe_url, validate_recipe_url, AcknowledgeMessagesRequest,
    AgentCommand, AgentInfo, AgentLaunchProfile, AppDeployment, AppDeploymentStatus,
    AppDesiredState, AppIdentityVerifyRequest, AppPrincipal, AppPrincipalKind,
    CreateAgentLaunchProfileRequest, CreateAgentRequest, CreateAppDeploymentRequest,
    CreateMachineServiceRequest, CreateServiceIngressRequest, CreateVirtualNetworkHostRequest,
    GetMessageResponse, ImportMessagesRequest, InputAgentRequest, LaunchAgentProfileRequest,
    ListMessagesQuery, MachineEnrollmentRequest, MachineEnrollmentResponse, MachineExecRequest,
    MachineService, MessagePrincipal, MessagePrincipalKind, PromptAgentRequest, ProtocolError,
    ReceiveMessagesRequest, RenameRequest, ResolveAppRecipientsRequest,
    ResolveAppRecipientsResponse, SendMessageRequest, ServerStatus, ServiceIngress,
    ServiceIngressAccess, SetAgentStartupRequest, TerminalClientMessage, TerminalCursor,
    TerminalServerMessage, UpdateAgentLaunchProfileRequest, UpdateMachineServiceRequest,
    UpdateServiceIngressRequest, UploadMachineFileRequest, UploadMachineFileResponse,
    ValidateAgentStartupRequest, ValidateAgentStartupResponse, VirtualNetworkHostsSnapshot,
    WorkloadIdentityTokenRequest, WorkloadIdentityVerifyRequest, WorkspaceEvent, WorkspaceSnapshot,
    AGENT_ID_HEADER,
};
use url::Url;
use uuid::Uuid;

use crate::admin;
use crate::agent_socket;
use crate::audit::NewWorkspaceAuditEvent;
use crate::auth::{
    self, managed_app_ingress_hostname, AuthStore, CurrentSession, MachineSession,
    ProfileMutationActor,
};
use crate::identity::IdentityIssuer;
use crate::message_store::MessageStore;
use crate::policy::{
    PolicyEngine, PolicyRequest, PolicyResource, PolicySubject, ACTION_AGENT_ABORT,
    ACTION_AGENT_CREATE, ACTION_AGENT_DELETE, ACTION_AGENT_DISCOVER, ACTION_AGENT_INPUT,
    ACTION_AGENT_METADATA_READ, ACTION_AGENT_OUTPUT_READ, ACTION_AGENT_PROMPT,
    ACTION_AGENT_STARTUP_MANAGE, ACTION_AGENT_STARTUP_READ, ACTION_AGENT_STOP, ACTION_AGENT_UPDATE,
    ACTION_HUMAN_LIST, ACTION_IDENTITY_TOKEN_ISSUE, ACTION_INGRESS_LIST,
    ACTION_LAUNCH_PROFILE_CREATE, ACTION_LAUNCH_PROFILE_DELETE, ACTION_LAUNCH_PROFILE_LIST,
    ACTION_LAUNCH_PROFILE_READ, ACTION_LAUNCH_PROFILE_UPDATE, ACTION_LAUNCH_PROFILE_USE,
    ACTION_MACHINE_DELETE, ACTION_MACHINE_EXEC, ACTION_MACHINE_FILE_WRITE, ACTION_MACHINE_UPDATE,
    ACTION_MESSAGE_ACK, ACTION_MESSAGE_IMPORT, ACTION_MESSAGE_READ, ACTION_MESSAGE_RECEIVE,
    ACTION_MESSAGE_SEND, ACTION_SERVICE_LIST, ACTION_SERVICE_PROBE, ACTION_VIRTUAL_HOST_LIST,
    RESOURCE_AGENT, RESOURCE_AGENT_LAUNCH_PROFILE, RESOURCE_HUMAN_DIRECTORY, RESOURCE_MACHINE,
    RESOURCE_MACHINE_SERVICE, RESOURCE_MESSAGE, RESOURCE_MESSAGE_DELIVERY, RESOURCE_MESSAGE_IMPORT,
    RESOURCE_MESSAGE_MAILBOX, RESOURCE_SERVICE_INGRESS, RESOURCE_VIRTUAL_HOST,
};
use crate::state::{AppState, SocketFrame, TERMINAL_BROWSER_QUEUE_CAPACITY};
use crate::traffic::TrafficClass;
use crate::updater::UpdaterClient;
use crate::voice::{VoiceAsrConfig, VoiceServices};
use crate::voice_llm::{self, VoiceCommandError, VoiceLlmConfig};

const TERMINAL_FLOW_WINDOW_BYTES: usize = 256 * 1024;
const MAX_MACHINE_FILE_BYTES: usize = 16 * 1024 * 1024;
const MACHINE_FILE_CHUNK_BYTES: usize = 192 * 1024;
const MAX_MACHINE_UPLOAD_BODY_BYTES: usize = 23 * 1024 * 1024;

fn control_audit_actor<'a>(
    session: Option<&'a CurrentSession>,
    subject: Option<&'a PolicySubject>,
) -> (&'static str, Option<&'a str>) {
    if let Some(session) = session {
        return ("user", Some(session.user_id.as_str()));
    }
    match subject {
        Some(PolicySubject::Agent { agent_id, .. }) => ("agent", Some(agent_id.as_str())),
        Some(PolicySubject::Machine { server_id }) => ("machine", Some(server_id.as_str())),
        Some(PolicySubject::Human { user_id }) => ("human", Some(user_id.as_str())),
        Some(PolicySubject::Service { service_id }) => ("service", Some(service_id.as_str())),
        None => ("system", None),
    }
}

#[derive(Clone)]
pub struct BootstrapConfig {
    public_url: Url,
    artifacts_dir: PathBuf,
    release_artifact_base_url: Url,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityRollout {
    core_messages: bool,
}

impl CapabilityRollout {
    pub const fn new(core_messages: bool) -> Self {
        Self { core_messages }
    }

    #[cfg(test)]
    const fn all_enabled() -> Self {
        Self::new(true)
    }
}

#[derive(Clone)]
pub struct BrowserAccess {
    origin: HeaderValue,
    origin_text: Arc<str>,
    proxy_origin: HeaderValue,
}

#[derive(Clone)]
struct WorkloadIdentityApi {
    auth: AuthStore,
    policy: PolicyEngine,
    issuer: IdentityIssuer,
}

#[derive(Clone)]
struct ServiceIngressApi {
    auth: AuthStore,
    policy: PolicyEngine,
    config: IngressConfig,
}

const INGRESS_SESSION_COOKIE: &str = "__Host-treer_ingress";
const TREER_AUTHORIZATION_HEADER: &str = "treer-authorization";
const TREER_IDENTITY_TOKEN_HEADER: &str = "x-treer-identity-token";

#[derive(Clone)]
pub struct IngressConfig {
    public_url: Option<Url>,
    base_domain: Option<Arc<str>>,
    proxy_public_url: Url,
    app_public_url: Url,
}

impl IngressConfig {
    pub fn new(
        mut public_url: Option<Url>,
        proxy_public_url: &Url,
        app_public_url: &Url,
    ) -> anyhow::Result<Self> {
        let base_domain = if let Some(url) = public_url.as_mut() {
            if !matches!(url.scheme(), "http" | "https")
                || url.username() != ""
                || url.password().is_some()
            {
                anyhow::bail!("ingress public URL must be an HTTP(S) URL without credentials");
            }
            let hostname = url
                .host_str()
                .context("ingress public URL must contain a base domain")?
                .trim_end_matches('.')
                .to_ascii_lowercase();
            let valid = hostname.len() <= 253
                && hostname.split('.').count() >= 2
                && hostname.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        })
                });
            if !valid {
                anyhow::bail!("ingress public URL must use a valid DNS base domain");
            }
            url.set_path("/");
            url.set_query(None);
            url.set_fragment(None);
            Some(hostname.into())
        } else {
            None
        };
        Ok(Self {
            public_url,
            base_domain,
            proxy_public_url: proxy_public_url.clone(),
            app_public_url: app_public_url.clone(),
        })
    }

    pub fn public_url(&self) -> Option<&Url> {
        self.public_url.as_ref()
    }

    pub fn base_domain_if_configured(&self) -> Option<&str> {
        self.base_domain.as_deref()
    }

    fn base_domain(&self) -> Result<&str, ApiFailure> {
        self.base_domain.as_deref().ok_or_else(|| {
            ApiFailure::service_unavailable(
                "ingress_not_configured",
                "TREER_INGRESS_PUBLIC_URL is not configured",
            )
        })
    }

    fn matches_hostname(&self, hostname: &str) -> bool {
        self.base_domain.as_deref().is_some_and(|base| {
            hostname
                .strip_suffix(&format!(".{base}"))
                .is_some_and(|label| !label.is_empty() && !label.contains('.'))
        })
    }

    fn url_for_hostname(&self, hostname: &str) -> Result<Url, ApiFailure> {
        let mut url = self.public_url.clone().ok_or_else(|| {
            ApiFailure::service_unavailable(
                "ingress_not_configured",
                "TREER_INGRESS_PUBLIC_URL is not configured",
            )
        })?;
        url.set_host(Some(hostname)).map_err(|_| {
            ApiFailure::internal(
                "invalid_ingress_hostname",
                "stored ingress hostname is invalid",
            )
        })?;
        Ok(url)
    }

    fn ingress_cookie_name(&self) -> &'static str {
        if self
            .public_url
            .as_ref()
            .is_some_and(|url| url.scheme() == "https")
        {
            INGRESS_SESSION_COOKIE
        } else {
            "treer_ingress"
        }
    }
}

impl BootstrapConfig {
    pub fn new(
        public_url: Url,
        artifacts_dir: PathBuf,
        mut release_artifact_base_url: Url,
    ) -> Self {
        let mut path = release_artifact_base_url
            .path()
            .trim_end_matches('/')
            .to_string();
        path.push('/');
        release_artifact_base_url.set_path(&path);
        release_artifact_base_url.set_query(None);
        release_artifact_base_url.set_fragment(None);
        Self {
            public_url,
            artifacts_dir,
            release_artifact_base_url,
        }
    }
}

impl BrowserAccess {
    pub fn new(app_public_url: &Url, proxy_public_url: &Url) -> anyhow::Result<Self> {
        let origin_text: Arc<str> = app_public_url.origin().ascii_serialization().into();
        let origin = HeaderValue::from_str(&origin_text)
            .context("app public URL produced an invalid HTTP Origin")?;
        let proxy_origin = HeaderValue::from_str(&proxy_public_url.origin().ascii_serialization())
            .context("proxy public URL produced an invalid HTTP Origin")?;
        Ok(Self {
            origin,
            origin_text,
            proxy_origin,
        })
    }

    fn cors_layer(&self) -> CorsLayer {
        CorsLayer::new()
            .allow_origin(self.origin.clone())
            .allow_credentials(true)
            .allow_methods([
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PATCH,
                Method::DELETE,
            ])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
    }

    fn validate_if_present(&self, headers: &HeaderMap) -> Result<(), ApiFailure> {
        let Some(origin) = headers.get(header::ORIGIN) else {
            return Ok(());
        };
        if origin == self.origin {
            Ok(())
        } else {
            Err(ApiFailure::forbidden(
                "browser_origin_denied",
                &format!("browser requests must originate from {}", self.origin_text),
            ))
        }
    }

    fn validate_tunnel_if_present(&self, headers: &HeaderMap) -> Result<(), ApiFailure> {
        let Some(origin) = headers.get(header::ORIGIN) else {
            return Ok(());
        };
        if origin == self.origin || origin == self.proxy_origin {
            Ok(())
        } else {
            Err(ApiFailure::forbidden(
                "browser_origin_denied",
                "browser tunnel request has an unrecognized origin",
            ))
        }
    }
}

mod agents;
mod app_identity;
mod apps;
mod errors;
mod events;
mod machines;
mod messages;
mod network;
mod policy_provider;
mod routes;
mod voice;
mod workspaces;

use agents::*;
use app_identity::*;
pub(crate) use apps::reconcile_app_deployments_for_server;
use apps::*;
pub(crate) use errors::*;
use events::*;
use machines::*;
use messages::*;
use network::*;
pub(crate) use network::{spawn_network_metadata_refresh, virtual_network_hosts_snapshot};
use policy_provider::*;
use voice::*;
use workspaces::*;

#[allow(clippy::too_many_arguments)]
pub fn router(
    state: AppState,
    bootstrap: BootstrapConfig,
    auth_store: AuthStore,
    policy: PolicyEngine,
    identity: IdentityIssuer,
    browser: BrowserAccess,
    ingress: IngressConfig,
    messages: MessageStore,
    rollout: CapabilityRollout,
    updater: UpdaterClient,
    voice: VoiceServices,
) -> Router {
    let provider_store = crate::policy_provider_store::PolicyProviderStore::new(auth_store.pool());
    routes::router(
        state,
        bootstrap,
        auth_store,
        policy,
        provider_store,
        identity,
        browser,
        ingress,
        messages,
        rollout,
        updater,
        voice,
    )
}

async fn core_messages_rollout_disabled() -> ApiFailure {
    ApiFailure::service_unavailable(
        "core_messages_disabled",
        "Core Message routes are disabled until rollout prerequisites pass",
    )
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "treer-proxy" }))
}

async fn workload_identity_jwks(Extension(identity): Extension<IdentityIssuer>) -> Response {
    (
        [(header::CACHE_CONTROL, "public, max-age=300")],
        Json(identity.jwks()),
    )
        .into_response()
}

async fn verify_workload_identity(
    Extension(identity): Extension<IdentityIssuer>,
    Json(request): Json<WorkloadIdentityVerifyRequest>,
) -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(identity.verify(&request.token, request.audience.trim())),
    )
        .into_response()
}

#[cfg(test)]
#[path = "api/tests.rs"]
mod tests;
