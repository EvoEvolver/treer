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
use axum::routing::{any, get, patch, post};
use axum::{Json, Router};
use base64::Engine;
#[cfg(test)]
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
#[cfg(test)]
use treer_protocol::ApiError;
use treer_protocol::{
    AcknowledgeMessagesRequest, AgentCommand, AgentInfo, AgentLaunchProfile, AppDeployment,
    AppDeploymentStatus, AppDesiredState, AppIdentityVerifyRequest, AppPrincipal, AppPrincipalKind,
    CreateAgentLaunchProfileRequest, CreateAgentRequest, CreateAppDeploymentRequest,
    CreateMachineServiceRequest, CreateServiceIngressRequest, CreateVirtualNetworkHostRequest,
    GetMessageResponse, ImportMessagesRequest, InputAgentRequest, LaunchAgentProfileRequest,
    ListMessagesQuery, MachineEnrollmentRequest, MachineEnrollmentResponse, MachineExecRequest,
    MachineService, MessagePrincipal, MessagePrincipalKind, ProtocolError, ReceiveMessagesRequest,
    RenameRequest, ReplaceAgentStartupRequest, ResolveAppRecipientsRequest,
    ResolveAppRecipientsResponse, SendMessageRequest, ServerStatus, ServiceIngress,
    ServiceIngressAccess, SetAgentStartupRequest, TerminalClientMessage, TerminalCursor,
    TerminalServerMessage, UpdateAgentLaunchProfileRequest, UpdateMachineServiceRequest,
    UpdateServiceIngressRequest, UploadMachineFileRequest, UploadMachineFileResponse,
    ValidateAgentStartupRequest, ValidateAgentStartupResponse, VirtualNetworkHostsSnapshot,
    WorkloadIdentityTokenRequest, WorkloadIdentityVerifyRequest, WorkspaceEvent, AGENT_ID_HEADER,
};
use url::Url;
use uuid::Uuid;

use crate::admin;
use crate::agent_socket;
use crate::audit::NewWorkspaceAuditEvent;
use crate::auth::{self, AuthStore, CurrentSession, MachineSession, ProfileMutationActor};
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
mod policy_diagnostics;
use policy_diagnostics::*;
mod policy_publish_authority;
mod workspace_audit;
use policy_publish_authority::*;

const TERMINAL_FLOW_WINDOW_BYTES: usize = 256 * 1024;
const MAX_MACHINE_FILE_BYTES: usize = 16 * 1024 * 1024;
const MACHINE_FILE_CHUNK_BYTES: usize = 192 * 1024;
const MAX_MACHINE_UPLOAD_BODY_BYTES: usize = 23 * 1024 * 1024;
/// One chunked-upload part (8 MiB) plus slack; the handler enforces the exact
/// Content-Length rules before reading.
const MAX_MACHINE_UPLOAD_PART_BODY_BYTES: usize = 9 * 1024 * 1024;

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
    app_dist: AppDistConfig,
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
const INGRESS_EMBED_SESSION_COOKIE: &str = "__Host-treer_ingress_embed";
const TREER_AUTHORIZATION_HEADER: &str = "treer-authorization";
const TREER_IDENTITY_TOKEN_HEADER: &str = "x-treer-identity-token";

/// What the Console may do when an embed session cannot be established in
/// the browser (third-party cookies blocked): fall back to the Proxy-origin
/// tunnel, which shares the Proxy origin with the Agent page, or refuse so
/// the Console offers "open in new tab" only (#156, security 4701 (8)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EmbedFallback {
    #[default]
    Tunnel,
    Refuse,
}

impl std::str::FromStr for EmbedFallback {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tunnel" => Ok(Self::Tunnel),
            "refuse" => Ok(Self::Refuse),
            other => anyhow::bail!("embed fallback must be 'tunnel' or 'refuse', not {other:?}"),
        }
    }
}

#[derive(Clone)]
pub struct IngressConfig {
    public_url: Option<Url>,
    base_domain: Option<Arc<str>>,
    proxy_public_url: Url,
    app_public_url: Url,
    /// Admission control for public ingress (#146); defaults to no trusted
    /// front, so the TCP peer is the client.
    guard: crate::ingress_guard::IngressGuard,
    embed_fallback: EmbedFallback,
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
            guard: crate::ingress_guard::IngressGuard::new(
                crate::ingress_guard::TrustedProxies::none(),
            ),
            embed_fallback: EmbedFallback::default(),
        })
    }

    /// Replaces the default guard with one built from operator configuration.
    pub fn with_guard(mut self, guard: crate::ingress_guard::IngressGuard) -> Self {
        self.guard = guard;
        self
    }

    pub fn with_embed_fallback(mut self, fallback: EmbedFallback) -> Self {
        self.embed_fallback = fallback;
        self
    }

    pub fn embed_fallback(&self) -> EmbedFallback {
        self.embed_fallback
    }

    /// Embed sessions need a wildcard base domain for the `ag-` origins and
    /// HTTPS, because their cookie is `Partitioned` + `SameSite=None`, which
    /// browsers accept only with `Secure`.
    pub(crate) fn embed_available(&self) -> bool {
        self.base_domain.is_some()
            && self
                .public_url
                .as_ref()
                .is_some_and(|url| url.scheme() == "https")
    }

    /// The Console origin (scheme, host, port) that may frame embed sessions.
    pub(in crate::api) fn console_origin(&self) -> String {
        self.app_public_url.origin().ascii_serialization()
    }

    pub fn guard(&self) -> &crate::ingress_guard::IngressGuard {
        &self.guard
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

    pub(in crate::api) fn matches_hostname(&self, hostname: &str) -> bool {
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

    /// The standalone origin of an Agent's interface UI (#143 Phase 2), or
    /// `None` without a configured apps domain.
    pub(in crate::api) fn agent_interface_origin_url(&self, agent_id: &str) -> Option<String> {
        let base = self.base_domain.as_deref()?;
        let hostname = crate::auth::platform_agent_hostname(agent_id, base).ok()?;
        self.url_for_hostname(&hostname)
            .ok()
            .map(|url| url.to_string())
    }

    /// The partitioned cookie of an embed session (#156); host-only like the
    /// standalone cookie, under a different name so a token can never be
    /// presented through the other cookie.
    pub(in crate::api) fn embed_cookie_name(&self) -> &'static str {
        INGRESS_EMBED_SESSION_COOKIE
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
            app_dist: AppDistConfig::disabled(),
        }
    }

    /// Serve the browser App build from this Proxy (single-container mode).
    pub fn with_app_dist(mut self, app_dist: AppDistConfig) -> Self {
        self.app_dist = app_dist;
        self
    }

    pub fn app_dist(&self) -> &AppDistConfig {
        &self.app_dist
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
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
            ])
            .allow_headers([
                header::CONTENT_TYPE,
                header::AUTHORIZATION,
                axum::http::HeaderName::from_static("x-treer-console"),
                // Chunked machine upload (#166/#173): begin carries the
                // Idempotency-Key, every part carries its SHA-256; the browser
                // preflights both because the Console and the API are on
                // different origins.
                axum::http::HeaderName::from_static("idempotency-key"),
                axum::http::HeaderName::from_static("x-treer-part-sha256"),
            ])
            // The upload client reads the retry hint on 409/429 replies.
            .expose_headers([header::RETRY_AFTER])
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
mod app_dist;
pub use app_dist::AppDistConfig;
mod app_identity;
mod app_ui;
mod apps;
mod errors;
mod events;
mod inventory_view;
mod machines;
mod messages;
mod network;
mod pins;
mod policy_provider;
mod route_catalog;
mod routes;
pub(crate) use route_catalog::RegisteredRoutes;
mod workload_tree;
mod workloads;
mod workspaces;

use agents::*;
use app_identity::*;
use app_ui::*;
pub(crate) use apps::synchronize_app_lifecycles_for_server;
use apps::*;
pub(crate) use errors::*;
use events::*;
use machines::*;
use messages::*;
use network::*;
pub(crate) use network::{spawn_network_metadata_refresh, virtual_network_hosts_snapshot};
use policy_provider::*;
use workload_tree::*;
use workloads::*;
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
    )
}

/// Starts the periodic machine upload expiry (#166) on this instance.
pub fn spawn_machine_upload_expiry(state: AppState, auth: AuthStore) {
    let store = crate::upload_store::UploadStore::new(auth.pool());
    agents::spawn_upload_expiry_task(state, auth, store);
}

async fn core_messages_rollout_disabled() -> ApiFailure {
    ApiFailure::service_unavailable(
        "core_messages_disabled",
        "Core Message routes are disabled until rollout prerequisites pass",
    )
}

async fn health() -> Json<Value> {
    Json(
        json!({ "status": "ok", "service": "treer-proxy", "capabilities": [treer_protocol::CAPABILITY_WORKLOAD_CREATE_PLACEMENT, treer_protocol::CAPABILITY_POLICY_PUBLISH_AUTHORITY, treer_protocol::CAPABILITY_MACHINE_FILE_TRANSFER, treer_protocol::CAPABILITY_APP_UPDATE, treer_protocol::controller_update::CAPABILITY, treer_protocol::CAPABILITY_WORKSPACE_PINS, treer_protocol::CAPABILITY_INGRESS_PROTECTION, treer_protocol::CAPABILITY_AGENT_INTERFACE_ORIGIN, treer_protocol::CAPABILITY_MACHINE_UPLOAD_V3] }),
    )
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
#[path = "api/agent_origin_tests.rs"]
mod agent_origin_tests;
#[cfg(test)]
#[path = "api/app_dist_tests.rs"]
mod app_dist_tests;
#[cfg(test)]
mod embed_session_tests;
#[cfg(test)]
#[path = "api/ingress_protection_tests.rs"]
mod ingress_protection_tests;
#[cfg(test)]
#[path = "api/pins_tests.rs"]
mod pins_tests;
#[cfg(test)]
#[path = "api/tests.rs"]
mod tests;
#[cfg(test)]
mod tunnel_revalidation_tests;
#[cfg(test)]
#[path = "api/upload_tests.rs"]
mod upload_tests;

#[cfg(test)]
mod app_identity_forwarding_tests;
