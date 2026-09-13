use std::collections::{HashMap, HashSet};
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
use treer_protocol::{
    installer_base_prompt, installer_composer_ready, pick_existing_installer_agent,
    recipe_installer_kind_allowed, recipe_url, validate_recipe_url, AcknowledgeMessagesRequest,
    AgentCommand, AgentInfo, AgentLaunchProfile, ApiError, AppDeployment, AppDeploymentStatus,
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
use crate::message_store::{MessageStore, MessageStoreError};
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
mod apps;
mod events;
mod machines;
mod messages;
mod network;
mod voice;
mod workspaces;

use agents::*;
pub(crate) use apps::reconcile_app_deployments_for_server;
use apps::*;
use events::*;
use machines::*;
use messages::*;
use network::*;
pub(crate) use network::{spawn_network_metadata_refresh, virtual_network_hosts_snapshot};
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
    let cors = browser.cors_layer();
    let workload_identity = WorkloadIdentityApi {
        auth: auth_store.clone(),
        policy: policy.clone(),
        issuer: identity.clone(),
    };
    let service_ingress = ServiceIngressApi {
        auth: auth_store.clone(),
        policy: policy.clone(),
        config: ingress.clone(),
    };
    let agent_control = Router::new()
        .route("/agent/machine/identity", post(bind_machine_identity))
        .route(
            "/agent/workspaces/{workspace_id}/snapshot",
            get(workspace_snapshot),
        )
        .route(
            "/agent/workspaces/{workspace_id}/identity/token",
            post(agent_issue_identity_token),
        )
        .route(
            "/agent/workspaces/{workspace_id}/humans",
            get(agent_list_humans),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents",
            get(list_agents).post(create_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps",
            get(list_app_deployments).post(create_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps/{app_id}",
            get(get_app_deployment).delete(delete_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps/{app_id}/start",
            post(start_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps/{app_id}/stop",
            post(stop_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps/{app_id}/restart",
            post(restart_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/launch-profiles",
            get(list_agent_launch_profiles).post(create_agent_launch_profile),
        )
        .route(
            "/agent/workspaces/{workspace_id}/launch-profiles/{profile_id}",
            get(get_agent_launch_profile)
                .patch(update_agent_launch_profile)
                .delete(delete_agent_launch_profile),
        )
        .route(
            "/agent/workspaces/{workspace_id}/launch-profiles/{profile_id}/launch",
            post(launch_agent_profile),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}",
            get(get_agent).patch(rename_agent).delete(delete_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/startup",
            get(get_agent_startup)
                .put(set_agent_startup)
                .delete(clear_agent_startup),
        )
        .route(
            "/agent/workspaces/{workspace_id}/machines/{server_id}/startup/validate",
            post(validate_agent_startup),
        )
        .route(
            "/agent/workspaces/{workspace_id}/servers/{server_id}",
            axum::routing::patch(rename_server).delete(delete_server),
        )
        .route(
            "/agent/workspaces/{workspace_id}/machines/{server_id}/exec",
            post(exec_machine),
        )
        .route(
            "/agent/workspaces/{workspace_id}/machines/{server_id}/files",
            post(upload_machine_file).layer(DefaultBodyLimit::max(MAX_MACHINE_UPLOAD_BODY_BYTES)),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services",
            get(agent_list_machine_services).post(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services/{service_id}",
            axum::routing::patch(agent_network_publication_forbidden)
                .delete(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services/{service_id}/probe",
            post(agent_probe_machine_service),
        )
        .route(
            "/agent/workspaces/{workspace_id}/virtual-hosts",
            get(agent_list_virtual_network_hosts).post(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/virtual-hosts/{hostname}",
            axum::routing::delete(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/ingresses",
            get(agent_list_service_ingresses).post(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/ingresses/{ingress_id}",
            axum::routing::patch(agent_network_publication_forbidden)
                .delete(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/prompt",
            post(prompt_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/input",
            post(input_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/output",
            get(read_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/transcript",
            get(read_agent_transcript),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/stop",
            post(stop_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/abort",
            post(abort_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/terminal",
            get(agent_terminal),
        );
    let agent_control = if rollout.core_messages {
        agent_control
            .route(
                "/agent/workspaces/{workspace_id}/messages",
                get(list_core_messages).post(send_core_message),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/receive",
                post(receive_core_messages),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/ack",
                post(acknowledge_core_messages),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/import",
                post(import_core_messages),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/{message_id}",
                get(get_core_message),
            )
    } else {
        agent_control
            .route(
                "/agent/workspaces/{workspace_id}/messages",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/receive",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/ack",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/import",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/{message_id}",
                any(core_messages_rollout_disabled),
            )
    };
    let agent_control = agent_control.route_layer(middleware::from_fn_with_state(
        auth_store.clone(),
        auth::require_machine,
    ));
    let app_messages = if rollout.core_messages {
        Router::new()
            .route(
                "/api/apps/{service_id}/messages",
                get(list_app_messages).post(send_app_message),
            )
            .route(
                "/api/apps/{service_id}/messages/receive",
                post(receive_app_messages),
            )
            .route(
                "/api/apps/{service_id}/messages/ack",
                post(acknowledge_app_messages),
            )
            .route(
                "/api/apps/{service_id}/messages/{message_id}",
                get(get_app_message),
            )
    } else {
        Router::new()
            .route(
                "/api/apps/{service_id}/messages",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/api/apps/{service_id}/messages/receive",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/api/apps/{service_id}/messages/ack",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/api/apps/{service_id}/messages/{message_id}",
                any(core_messages_rollout_disabled),
            )
    };
    let authenticated = Router::new()
        .route(
            "/api/organizations",
            get(auth::organizations).post(auth::create_organization_handler),
        )
        .route(
            "/api/organizations/{organization_id}",
            axum::routing::patch(auth::rename_organization_handler),
        )
        .route(
            "/api/organizations/{organization_id}/members",
            get(auth::members),
        )
        .route(
            "/api/organizations/{organization_id}/groups",
            get(auth::organization_groups).post(auth::create_organization_group_handler),
        )
        .route(
            "/api/organizations/{organization_id}/groups/{group_id}",
            axum::routing::delete(auth::delete_organization_group_handler),
        )
        .route(
            "/api/organizations/{organization_id}/groups/{group_id}/members/{user_id}",
            axum::routing::put(auth::add_organization_group_member_handler)
                .delete(auth::remove_organization_group_member_handler),
        )
        .route(
            "/api/organizations/{organization_id}/audit-events",
            get(auth::audit_events),
        )
        .route(
            "/api/organizations/{organization_id}/members/{user_id}",
            axum::routing::patch(auth::update_member_role_handler)
                .delete(auth::remove_member_handler),
        )
        .route(
            "/api/organizations/{organization_id}/invitations",
            post(auth::create_invitation),
        )
        .route(
            "/api/workspaces/{workspace_id}/bootstrap",
            post(bootstrap_info),
        )
        .route(
            "/api/workspaces",
            get(list_workspaces).post(create_workspace),
        )
        .route(
            "/api/workspaces/{workspace_id}",
            axum::routing::patch(rename_workspace).delete(delete_workspace),
        )
        .route(
            "/api/workspaces/{workspace_id}/access",
            get(auth::workspace_access).patch(auth::update_workspace_access),
        )
        .route(
            "/api/workspaces/{workspace_id}/access/users/{user_id}",
            axum::routing::put(auth::update_workspace_user_grant)
                .delete(auth::delete_workspace_user_grant),
        )
        .route(
            "/api/workspaces/{workspace_id}/access/groups/{group_id}",
            axum::routing::put(auth::update_workspace_group_grant)
                .delete(auth::delete_workspace_group_grant),
        )
        .route(
            "/api/workspaces/{workspace_id}/snapshot",
            get(workspace_snapshot),
        )
        .route("/api/workspaces/{workspace_id}/servers", get(list_servers))
        .route(
            "/api/workspaces/{workspace_id}/apps",
            get(list_app_deployments).post(create_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}",
            get(get_app_deployment).delete(delete_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/access",
            axum::routing::patch(update_app_access),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/start",
            post(start_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/stop",
            post(stop_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/restart",
            post(restart_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/services",
            get(list_machine_services).post(create_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/services/{service_id}",
            axum::routing::patch(update_machine_service).delete(delete_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/services/{service_id}/probe",
            post(probe_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts",
            get(list_virtual_network_hosts).post(create_virtual_network_host),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}",
            axum::routing::delete(delete_virtual_network_host),
        )
        .route(
            "/api/workspaces/{workspace_id}/ingresses",
            get(list_service_ingresses).post(create_service_ingress),
        )
        .route(
            "/api/workspaces/{workspace_id}/ingresses/{ingress_id}",
            axum::routing::patch(update_service_ingress).delete(delete_service_ingress),
        )
        .route(
            "/api/workspaces/{workspace_id}/traffic",
            get(list_machine_traffic),
        )
        .route(
            "/api/workspaces/{workspace_id}/traffic/agents",
            get(list_agent_traffic),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy",
            any(proxy_virtual_network_host_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/",
            any(proxy_virtual_network_host_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/{*path}",
            any(proxy_virtual_network_host_path),
        )
        .route(
            "/api/workspaces/{workspace_id}/servers/{server_id}",
            axum::routing::patch(rename_server).delete(delete_server),
        )
        .route(
            "/api/workspaces/{workspace_id}/machines/{server_id}/exec",
            post(exec_machine),
        )
        .route(
            "/api/workspaces/{workspace_id}/machines/{server_id}/files",
            post(upload_machine_file).layer(DefaultBodyLimit::max(MAX_MACHINE_UPLOAD_BODY_BYTES)),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents",
            get(list_agents).post(create_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/launch-profiles",
            get(list_agent_launch_profiles).post(create_agent_launch_profile),
        )
        .route(
            "/api/workspaces/{workspace_id}/launch-profiles/{profile_id}",
            get(get_agent_launch_profile)
                .patch(update_agent_launch_profile)
                .delete(delete_agent_launch_profile),
        )
        .route(
            "/api/workspaces/{workspace_id}/launch-profiles/{profile_id}/launch",
            post(launch_agent_profile),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}",
            get(get_agent).patch(rename_agent).delete(delete_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui",
            any(proxy_agent_interface_ui_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/",
            any(proxy_agent_interface_ui_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/{*path}",
            any(proxy_agent_interface_ui_path),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/prompt",
            post(prompt_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/input",
            post(input_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/output",
            get(read_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/transcript",
            get(read_agent_transcript),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/stop",
            post(stop_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/abort",
            post(abort_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/terminal",
            get(agent_terminal),
        )
        .route(
            "/api/workspaces/{workspace_id}/events",
            get(workspace_events),
        )
        .route(
            "/api/workspaces/{workspace_id}/voice/asr",
            get(voice_asr_status),
        )
        .route(
            "/api/workspaces/{workspace_id}/voice/asr/stream",
            get(voice_asr_stream),
        )
        .route(
            "/api/workspaces/{workspace_id}/voice/command",
            get(voice_command_status).post(voice_command),
        )
        .route("/api/auth/me", get(auth::me))
        .route(
            "/api/auth/profile",
            axum::routing::patch(auth::update_profile),
        )
        .route("/api/auth/logout", post(auth::logout))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_workspace_access,
        ))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_user,
        ));
    let admin = admin::routes()
        .route("/api/admin/me", get(auth::admin_me))
        .route("/api/admin/logout", post(auth::admin_logout))
        .route(
            "/api/admin/update",
            get(crate::updater::status).post(crate::updater::apply),
        )
        .route("/api/admin/update/check", get(crate::updater::check))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_admin,
        ));
    let control = Router::new()
        .route("/install.sh", get(install_script))
        .route("/api/machines/enroll", post(enroll_machine))
        .route("/artifacts/{platform}/{binary}", get(download_artifact))
        .route("/api/health", get(health))
        .route("/.well-known/jwks.json", get(workload_identity_jwks))
        .route("/.treer/identity/verify", post(verify_workload_identity))
        .route("/.treer/apps/identity/verify", post(verify_app_identity))
        .route("/api/apps/oauth/authorize", get(authorize_workspace_app))
        .route("/api/apps/oauth/token", post(exchange_workspace_app_code))
        .route(
            "/api/apps/{service_id}/directory",
            get(workspace_app_directory),
        )
        .route(
            "/api/apps/{service_id}/recipients/resolve",
            post(resolve_workspace_app_recipients),
        )
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/config", get(auth::oauth_config))
        .route("/api/auth/oauth/{provider}/start", get(auth::oauth_start))
        .route(
            "/api/auth/oauth/{provider}/callback",
            get(auth::oauth_callback),
        )
        .route(
            "/api/auth/request-password-reset",
            post(auth::request_password_reset),
        )
        .route("/api/auth/reset-password", post(auth::reset_password))
        .route("/api/auth/register", post(auth::register))
        .route("/api/admin/login", post(auth::admin_login))
        .route("/agent/connect", get(agent_socket::upgrade))
        .merge(agent_control)
        .merge(app_messages)
        .merge(authenticated)
        .merge(admin)
        .layer(cors);
    Router::new()
        .merge(control)
        .route("/.treer/ingress/authorize", get(authorize_service_ingress))
        .fallback(any(proxy_service_ingress))
        .layer(Extension(bootstrap))
        .layer(Extension(policy))
        .layer(Extension(identity))
        .layer(Extension(workload_identity))
        .layer(Extension(service_ingress))
        .layer(Extension(auth_store))
        .layer(Extension(browser))
        .layer(Extension(ingress))
        .layer(Extension(messages))
        .layer(Extension(updater))
        .layer(Extension(voice.asr))
        .layer(Extension(voice.llm))
        .with_state(state)
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

async fn verify_app_identity(
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Json(request): Json<AppIdentityVerifyRequest>,
) -> Response {
    let mut verified = identity.verify_app(&request.token, request.audience.trim());
    if let Some(claims) = verified.claims.as_mut() {
        let service_active = auth
            .resolve_machine_service(&claims.workspace_id, &claims.service_id)
            .await
            .is_ok();
        let membership_active = claims.principal_kind != AppPrincipalKind::Human
            || match auth
                .workspace_member_role(&claims.workspace_id, &claims.sub)
                .await
            {
                Ok(role) => {
                    claims.role = Some(role);
                    true
                }
                Err(_) => false,
            };
        if !service_active || !membership_active {
            verified.active = false;
            verified.claims = None;
        }
    }
    ([(header::CACHE_CONTROL, "no-store")], Json(verified)).into_response()
}

#[derive(Debug, Deserialize)]
struct AppOAuthAuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
}

#[derive(Debug, Deserialize)]
struct AppOAuthTokenRequest {
    grant_type: String,
    code: String,
    client_id: String,
    redirect_uri: String,
    code_verifier: String,
}

async fn authorize_workspace_app(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    headers: HeaderMap,
    OriginalUri(original_uri): OriginalUri,
    Query(query): Query<AppOAuthAuthorizeQuery>,
) -> Result<Response, ApiFailure> {
    if query.response_type != "code"
        || query.code_challenge_method != "S256"
        || query.state.is_empty()
        || query.state.len() > 512
    {
        return Err(ApiFailure::bad_request(
            "invalid_app_oauth_request",
            "app OAuth requires response_type=code, S256 PKCE, and a bounded state",
        ));
    }
    let (redirect_uri, resolved) = resolve_app_redirect(
        &auth,
        &config,
        query.client_id.trim(),
        query.redirect_uri.trim(),
    )
    .await?;
    let session = match auth::authenticate_request(&auth, &headers).await {
        Ok(session) => session,
        Err(error) => {
            let (status, error) = error.into_parts();
            if status != StatusCode::UNAUTHORIZED {
                return Err(ApiFailure { status, error });
            }
            let mut return_to = config.proxy_public_url.clone();
            return_to.set_path(original_uri.path());
            return_to.set_query(original_uri.query());
            let mut login = config.app_public_url.clone();
            login.set_query(None);
            login
                .query_pairs_mut()
                .append_pair("return_to", return_to.as_str());
            return Ok(Redirect::to(login.as_str()).into_response());
        }
    };
    let role = auth
        .workspace_member_role(&resolved.ingress.workspace_id, &session.user_id)
        .await?;
    let code = auth
        .create_app_oauth_code(
            &auth::AppOAuthGrant {
                workspace_id: resolved.ingress.workspace_id,
                service_id: resolved.service.service_id,
                user_id: session.user_id,
                preferred_name: session.preferred_name,
                role,
            },
            redirect_uri.as_str(),
            query.code_challenge.trim(),
        )
        .await?;
    let mut callback = redirect_uri;
    callback
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &query.state);
    Ok(Redirect::to(callback.as_str()).into_response())
}

async fn exchange_workspace_app_code(
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Form(request): Form<AppOAuthTokenRequest>,
) -> Result<Response, ApiFailure> {
    if request.grant_type != "authorization_code" {
        return Err(ApiFailure::bad_request(
            "unsupported_grant_type",
            "app OAuth supports only the authorization_code grant",
        ));
    }
    let grant = auth
        .consume_app_oauth_code(
            request.code.trim(),
            request.client_id.trim(),
            request.redirect_uri.trim(),
            request.code_verifier.trim(),
        )
        .await?;
    let token = identity
        .issue_human(
            &grant.workspace_id,
            &grant.user_id,
            &grant.preferred_name,
            &grant.role,
            &grant.service_id,
        )
        .map_err(|error| {
            tracing::error!(%error, "failed to sign app human identity token");
            ApiFailure::internal(
                "identity_signing_failed",
                "failed to sign app identity token",
            )
        })?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(token)).into_response())
}

#[allow(clippy::too_many_arguments)]
async fn workspace_app_directory(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiFailure> {
    let claims = authenticate_workspace_app(&auth, &identity, &headers, &service_id).await?;
    let principals = workspace_app_principals(&state, &auth, &claims.workspace_id).await?;
    Ok(Json(json!({ "principals": principals })))
}

async fn resolve_workspace_app_recipients(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ResolveAppRecipientsRequest>,
) -> Result<Json<ResolveAppRecipientsResponse>, ApiFailure> {
    if request.recipients.is_empty() || request.recipients.len() > 32 {
        return Err(ApiFailure::bad_request(
            "invalid_app_recipients",
            "recipient resolution requires 1-32 targets",
        ));
    }
    let claims = authenticate_workspace_app(&auth, &identity, &headers, &service_id).await?;
    let principals = workspace_app_principals(&state, &auth, &claims.workspace_id).await?;
    let sender = principals
        .iter()
        .find(|principal| principal.id == claims.sub && principal.kind == claims.principal_kind)
        .cloned()
        .unwrap_or(AppPrincipal {
            kind: claims.principal_kind,
            id: claims.sub.clone(),
            name: claims.name.clone(),
            role: claims.role.clone(),
        });
    let mut seen = HashSet::new();
    let mut recipients = Vec::new();
    for raw_target in request.recipients {
        let target = raw_target.trim();
        let target = if matches!(target, "self" | ".") {
            claims.sub.as_str()
        } else {
            target
        };
        let recipient = resolve_app_principal(&principals, target)?;
        if seen.insert((recipient.kind, recipient.id.clone())) {
            recipients.push(recipient);
        }
    }
    Ok(Json(ResolveAppRecipientsResponse { sender, recipients }))
}

async fn resolve_app_redirect(
    auth: &AuthStore,
    config: &IngressConfig,
    service_id: &str,
    redirect_uri: &str,
) -> Result<(Url, auth::ResolvedServiceIngress), ApiFailure> {
    let redirect = Url::parse(redirect_uri)
        .map_err(|_| ApiFailure::bad_request("invalid_redirect_uri", "redirect URI is invalid"))?;
    if !matches!(redirect.scheme(), "http" | "https")
        || redirect.host_str().is_none()
        || !redirect.username().is_empty()
        || redirect.password().is_some()
        || redirect.fragment().is_some()
    {
        return Err(ApiFailure::bad_request(
            "invalid_redirect_uri",
            "redirect URI must be an absolute HTTP URL without credentials or a fragment",
        ));
    }
    let hostname = redirect.host_str().expect("redirect host checked above");
    let resolved = auth
        .resolve_service_ingress_hostname(hostname)
        .await?
        .filter(|resolved| {
            resolved.ingress.enabled
                && resolved.ingress.access == ServiceIngressAccess::Workspace
                && resolved.service.service_id == service_id
        })
        .ok_or_else(|| {
            ApiFailure::bad_request(
                "invalid_redirect_uri",
                "redirect URI is not an enabled workspace ingress for this service",
            )
        })?;
    let expected_origin = config.url_for_hostname(hostname)?.origin();
    if redirect.origin() != expected_origin {
        return Err(ApiFailure::bad_request(
            "invalid_redirect_uri",
            "redirect URI origin does not match the registered service ingress",
        ));
    }
    Ok((redirect, resolved))
}

async fn authenticate_workspace_app(
    auth: &AuthStore,
    identity: &IdentityIssuer,
    headers: &HeaderMap,
    service_id: &str,
) -> Result<treer_protocol::AppIdentityClaims, ApiFailure> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiFailure::unauthorized(
                "app_authentication_required",
                "a service-audience Treer identity token is required",
            )
        })?;
    let mut claims = identity
        .verify_app(token, service_id)
        .claims
        .ok_or_else(|| {
            ApiFailure::unauthorized(
                "app_authentication_required",
                "the Treer identity token is invalid for this service",
            )
        })?;
    auth.resolve_machine_service(&claims.workspace_id, service_id)
        .await
        .map_err(|_| {
            ApiFailure::unauthorized(
                "app_authentication_required",
                "the target workspace service is no longer active",
            )
        })?;
    if claims.principal_kind == AppPrincipalKind::Human {
        claims.role = Some(
            auth.workspace_member_role(&claims.workspace_id, &claims.sub)
                .await
                .map_err(|_| {
                    ApiFailure::unauthorized(
                        "app_authentication_required",
                        "the human identity is no longer a workspace member",
                    )
                })?,
        );
    }
    Ok(claims)
}

async fn workspace_app_principals(
    state: &AppState,
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<Vec<AppPrincipal>, ApiFailure> {
    let snapshot = state.snapshot(workspace_id).await?;
    let humans = auth.list_workspace_humans(workspace_id).await?;
    Ok(snapshot
        .agents
        .into_iter()
        .map(|agent| AppPrincipal {
            kind: AppPrincipalKind::Agent,
            id: agent.agent_id,
            name: agent.name,
            role: None,
        })
        .chain(humans.into_iter().map(|human| AppPrincipal {
            kind: AppPrincipalKind::Human,
            id: human.user_id,
            name: human.preferred_name,
            role: Some(human.role),
        }))
        .collect())
}

fn resolve_app_principal(
    principals: &[AppPrincipal],
    target: &str,
) -> Result<AppPrincipal, ProtocolError> {
    let mut matches = principals
        .iter()
        .filter(|principal| principal.id == target)
        .cloned()
        .collect::<Vec<_>>();
    if matches.is_empty() {
        matches.extend(
            principals
                .iter()
                .filter(|principal| principal.name == target)
                .cloned(),
        );
    }
    match matches.as_slice() {
        [] => Err(ProtocolError::new(
            "recipient_not_found",
            format!("no Agent or human recipient matches {target}"),
        )),
        [recipient] => Ok(recipient.clone()),
        _ => Err(ProtocolError::new(
            "recipient_ambiguous",
            format!("more than one Agent or human is named {target}; use a stable id"),
        )),
    }
}

async fn app_message_identity(
    state: &AppState,
    auth: &AuthStore,
    identity: &IdentityIssuer,
    headers: &HeaderMap,
    service_id: &str,
) -> Result<(String, PolicySubject, MessagePrincipal), ApiFailure> {
    let claims = authenticate_workspace_app(auth, identity, headers, service_id).await?;
    let workspace_id = claims.workspace_id.clone();
    let (subject, principal) = match claims.principal_kind {
        AppPrincipalKind::Human => (
            PolicySubject::Human {
                user_id: claims.sub.clone(),
            },
            MessagePrincipal {
                kind: MessagePrincipalKind::Human,
                id: claims.sub,
                name: claims.name,
                role: claims.role,
            },
        ),
        AppPrincipalKind::Agent => {
            let server_id = claims.machine_id.ok_or_else(|| {
                ApiFailure::unauthorized(
                    "app_authentication_required",
                    "the Agent App identity is missing its machine binding",
                )
            })?;
            let agent = state
                .resolve_agent(&claims.workspace_id, &claims.sub)
                .await
                .map_err(|_| {
                    ApiFailure::unauthorized(
                        "app_authentication_required",
                        "the Agent App identity is no longer active",
                    )
                })?;
            if agent.server_id != server_id {
                return Err(ApiFailure::unauthorized(
                    "app_authentication_required",
                    "the Agent App identity no longer matches its machine",
                ));
            }
            (
                PolicySubject::Agent {
                    server_id,
                    agent_id: agent.agent_id.clone(),
                },
                MessagePrincipal {
                    kind: MessagePrincipalKind::Agent,
                    id: agent.agent_id,
                    name: agent.name,
                    role: None,
                },
            )
        }
    };
    Ok((workspace_id, subject, principal))
}

#[derive(Debug)]
pub struct ApiFailure {
    status: StatusCode,
    error: ProtocolError,
}

impl ApiFailure {
    pub(crate) fn unauthorized(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn forbidden(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn bad_request(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn not_found(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn bad_gateway(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn service_unavailable(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn internal(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: ProtocolError::new(code, message),
        }
    }
}

impl From<ProtocolError> for ApiFailure {
    fn from(error: ProtocolError) -> Self {
        let status = match error.code.as_str() {
            "workspace_not_found"
            | "server_not_found"
            | "agent_not_found"
            | "recipient_not_found" => StatusCode::NOT_FOUND,
            "workspace_exists" | "agent_ambiguous" | "server_ambiguous" | "recipient_ambiguous" => {
                StatusCode::CONFLICT
            }
            "machine_file_exists" => StatusCode::CONFLICT,
            "agent_startup_not_found" => StatusCode::NOT_FOUND,
            "policy_denied" | "policy_subject_mismatch" | "agent_identity_mismatch" => {
                StatusCode::FORBIDDEN
            }
            "server_offline" | "no_online_server" | "ssh_unsupported" | "scp_unsupported" => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            "invalid_agent_identity"
            | "invalid_name"
            | "invalid_request"
            | "invalid_machine_exec"
            | "invalid_machine_exec_timeout"
            | "invalid_machine_path"
            | "invalid_machine_file_name"
            | "invalid_machine_upload"
            | "machine_upload_chunk_too_large" => StatusCode::BAD_REQUEST,
            "invalid_agent_startup" | "invalid_agent_startup_validation" => StatusCode::BAD_REQUEST,
            _ => StatusCode::BAD_GATEWAY,
        };
        Self { status, error }
    }
}

impl From<auth::AuthFailure> for ApiFailure {
    fn from(error: auth::AuthFailure) -> Self {
        let (status, error) = error.into_parts();
        Self { status, error }
    }
}

impl From<MessageStoreError> for ApiFailure {
    fn from(error: MessageStoreError) -> Self {
        match error {
            MessageStoreError::Contract { code, message } => {
                let status = match code {
                    "message_not_found"
                    | "message_context_not_found"
                    | "message_delivery_not_found" => StatusCode::NOT_FOUND,
                    "message_idempotency_conflict"
                    | "message_ack_idempotency_conflict"
                    | "message_import_idempotency_conflict"
                    | "message_import_conflict" => StatusCode::CONFLICT,
                    _ => StatusCode::BAD_REQUEST,
                };
                Self {
                    status,
                    error: ProtocolError::new(code, message),
                }
            }
            MessageStoreError::Database(_) => {
                tracing::error!("Core Message database operation failed");
                Self::service_unavailable(
                    "message_store_unavailable",
                    "Core Message storage is unavailable",
                )
            }
            MessageStoreError::Corrupt => {
                tracing::error!("Core Message storage returned invalid data");
                Self::internal(
                    "message_store_corrupt",
                    "Core Message storage contains invalid data",
                )
            }
        }
    }
}

impl From<serde_json::Error> for ApiFailure {
    fn from(error: serde_json::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: ProtocolError::new("encode_error", error.to_string()),
        }
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.error })).into_response()
    }
}

#[cfg(test)]
#[path = "api/tests.rs"]
mod tests;
