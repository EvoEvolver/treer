use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Form, OriginalUri, Path, Query, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, Version};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
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
    ListMessagesQuery, MachineEnrollmentRequest, MachineEnrollmentResponse, MachineService,
    MessagePrincipal, MessagePrincipalKind, PromptAgentRequest, ProtocolError,
    ReceiveMessagesRequest, RenameRequest, ResolveAppRecipientsRequest,
    ResolveAppRecipientsResponse, SendMessageRequest, ServerStatus, ServiceIngress,
    ServiceIngressAccess, TerminalClientMessage, TerminalCursor, TerminalServerMessage,
    UpdateAgentLaunchProfileRequest, UpdateMachineServiceRequest, UpdateServiceIngressRequest,
    VirtualNetworkHostsSnapshot, WorkloadIdentityTokenRequest, WorkloadIdentityVerifyRequest,
    WorkspaceEvent, WorkspaceSnapshot, AGENT_ID_HEADER,
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
    PolicyEngine, PolicyRequest, PolicyResource, PolicySubject, ACTION_AGENT_CREATE,
    ACTION_AGENT_DELETE, ACTION_AGENT_DISCOVER, ACTION_AGENT_INPUT, ACTION_AGENT_METADATA_READ,
    ACTION_AGENT_OUTPUT_READ, ACTION_AGENT_PROMPT, ACTION_AGENT_STOP, ACTION_AGENT_UPDATE,
    ACTION_HUMAN_LIST, ACTION_IDENTITY_TOKEN_ISSUE, ACTION_INGRESS_LIST,
    ACTION_LAUNCH_PROFILE_CREATE, ACTION_LAUNCH_PROFILE_DELETE, ACTION_LAUNCH_PROFILE_LIST,
    ACTION_LAUNCH_PROFILE_READ, ACTION_LAUNCH_PROFILE_UPDATE, ACTION_LAUNCH_PROFILE_USE,
    ACTION_MACHINE_DELETE, ACTION_MACHINE_UPDATE, ACTION_MESSAGE_ACK, ACTION_MESSAGE_IMPORT,
    ACTION_MESSAGE_READ, ACTION_MESSAGE_RECEIVE, ACTION_MESSAGE_SEND, ACTION_SERVICE_LIST,
    ACTION_SERVICE_PROBE, ACTION_VIRTUAL_HOST_LIST, RESOURCE_AGENT, RESOURCE_AGENT_LAUNCH_PROFILE,
    RESOURCE_HUMAN_DIRECTORY, RESOURCE_MACHINE, RESOURCE_MACHINE_SERVICE, RESOURCE_MESSAGE,
    RESOURCE_MESSAGE_DELIVERY, RESOURCE_MESSAGE_IMPORT, RESOURCE_MESSAGE_MAILBOX,
    RESOURCE_SERVICE_INGRESS, RESOURCE_VIRTUAL_HOST,
};
use crate::state::{AppState, SocketFrame, TERMINAL_BROWSER_QUEUE_CAPACITY};
use crate::traffic::TrafficClass;
use crate::updater::UpdaterClient;

const TERMINAL_FLOW_WINDOW_BYTES: usize = 256 * 1024;

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
            "/agent/workspaces/{workspace_id}/servers/{server_id}",
            axum::routing::patch(rename_server).delete(delete_server),
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
            "/api/workspaces/{workspace_id}/agents/{agent_id}/terminal",
            get(agent_terminal),
        )
        .route(
            "/api/workspaces/{workspace_id}/events",
            get(workspace_events),
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

#[allow(clippy::too_many_arguments)]
async fn send_app_message(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendMessageRequest>,
) -> Result<Json<treer_protocol::SendMessageResponse>, ApiFailure> {
    let (workspace_id, subject, sender) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        send_message_for(
            &state,
            &auth,
            &policy,
            &messages,
            &workspace_id,
            subject,
            sender,
            request,
        )
        .await?,
    ))
}

async fn get_app_message(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path((service_id, message_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<GetMessageResponse>, ApiFailure> {
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        get_message_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            &message_id,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn list_app_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(service_id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
    headers: HeaderMap,
) -> Result<Json<treer_protocol::MessagePage>, ApiFailure> {
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        list_messages_for(&policy, &messages, &workspace_id, subject, principal, query).await?,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn receive_app_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ReceiveMessagesRequest>,
) -> Result<Json<treer_protocol::ReceiveMessagesResponse>, ApiFailure> {
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        receive_messages_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn acknowledge_app_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AcknowledgeMessagesRequest>,
) -> Result<Json<treer_protocol::AcknowledgeMessagesResponse>, ApiFailure> {
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &headers, &service_id).await?;
    Ok(Json(
        acknowledge_messages_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn send_core_message(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendMessageRequest>,
) -> Result<Json<treer_protocol::SendMessageResponse>, ApiFailure> {
    let (subject, sender) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        send_message_for(
            &state,
            &auth,
            &policy,
            &messages,
            &workspace_id,
            subject,
            sender,
            request,
        )
        .await?,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn send_message_for(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    sender: MessagePrincipal,
    request: SendMessageRequest,
) -> Result<treer_protocol::SendMessageResponse, ApiFailure> {
    if request.recipients.is_empty() || request.recipients.len() > 32 {
        return Err(ApiFailure::bad_request(
            "message_recipients_invalid",
            "message requires 1-32 recipients",
        ));
    }
    let directory = workspace_app_principals(state, auth, workspace_id).await?;
    let mut recipients = Vec::with_capacity(request.recipients.len());
    for target in &request.recipients {
        let target = if matches!(target.trim(), "self" | ".") {
            sender.id.as_str()
        } else {
            target.trim()
        };
        let recipient = resolve_app_principal(&directory, target)
            .map(MessagePrincipal::from)
            .map_err(|_| message_recipient_unavailable())?;
        recipients.push(recipient);
    }
    let recipient_count = recipients.len();
    let mut policy_requests = recipients
        .iter()
        .map(|recipient| {
            PolicyRequest::new(
                workspace_id,
                subject.clone(),
                ACTION_MESSAGE_SEND,
                message_mailbox_policy_resource(recipient),
            )
        })
        .collect::<Vec<_>>();
    policy_requests.extend(request.context_ids.iter().map(|context_id| {
        PolicyRequest::new(
            workspace_id,
            subject.clone(),
            ACTION_MESSAGE_READ,
            PolicyResource::new(RESOURCE_MESSAGE, context_id),
        )
    }));
    let authorization = policy
        .authorize_batch(&policy_requests)
        .await
        .map_err(|denial| {
            if denial.request_index < recipient_count {
                message_recipient_unavailable()
            } else {
                ApiFailure::from(denial.error)
            }
        })?;
    Ok(messages
        .send_with_policy_revision(
            workspace_id,
            &sender,
            &recipients,
            &request,
            authorization.revision,
        )
        .await?)
}

#[allow(clippy::too_many_arguments)]
async fn get_core_message(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path((workspace_id, message_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<GetMessageResponse>, ApiFailure> {
    let (subject, principal) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        get_message_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            &message_id,
        )
        .await?,
    ))
}

async fn get_message_for(
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    principal: MessagePrincipal,
    message_id: &str,
) -> Result<GetMessageResponse, ApiFailure> {
    authorize_control(
        policy,
        workspace_id,
        Some(&subject),
        ACTION_MESSAGE_READ,
        PolicyResource::new(RESOURCE_MESSAGE, message_id),
    )
    .await?;
    Ok(GetMessageResponse {
        message: messages.get(workspace_id, &principal, message_id).await?,
    })
}

#[allow(clippy::too_many_arguments)]
async fn list_core_messages(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
    headers: HeaderMap,
) -> Result<Json<treer_protocol::MessagePage>, ApiFailure> {
    let (subject, principal) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        list_messages_for(&policy, &messages, &workspace_id, subject, principal, query).await?,
    ))
}

async fn list_messages_for(
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    principal: MessagePrincipal,
    query: ListMessagesQuery,
) -> Result<treer_protocol::MessagePage, ApiFailure> {
    authorize_control(
        policy,
        workspace_id,
        Some(&subject),
        ACTION_MESSAGE_READ,
        message_mailbox_policy_resource(&principal),
    )
    .await?;
    Ok(messages
        .list(
            workspace_id,
            &principal,
            query.before.as_deref(),
            query.limit,
        )
        .await?)
}

#[allow(clippy::too_many_arguments)]
async fn receive_core_messages(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ReceiveMessagesRequest>,
) -> Result<Json<treer_protocol::ReceiveMessagesResponse>, ApiFailure> {
    let (subject, principal) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        receive_messages_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

async fn receive_messages_for(
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    principal: MessagePrincipal,
    request: ReceiveMessagesRequest,
) -> Result<treer_protocol::ReceiveMessagesResponse, ApiFailure> {
    authorize_control(
        policy,
        workspace_id,
        Some(&subject),
        ACTION_MESSAGE_RECEIVE,
        message_mailbox_policy_resource(&principal),
    )
    .await?;
    Ok(messages
        .receive(
            workspace_id,
            &principal,
            request.limit,
            request.wait_milliseconds,
        )
        .await?)
}

#[allow(clippy::too_many_arguments)]
async fn acknowledge_core_messages(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AcknowledgeMessagesRequest>,
) -> Result<Json<treer_protocol::AcknowledgeMessagesResponse>, ApiFailure> {
    let (subject, principal) =
        message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    Ok(Json(
        acknowledge_messages_for(
            &policy,
            &messages,
            &workspace_id,
            subject,
            principal,
            request,
        )
        .await?,
    ))
}

async fn acknowledge_messages_for(
    policy: &PolicyEngine,
    messages: &MessageStore,
    workspace_id: &str,
    subject: PolicySubject,
    principal: MessagePrincipal,
    request: AcknowledgeMessagesRequest,
) -> Result<treer_protocol::AcknowledgeMessagesResponse, ApiFailure> {
    let policy_requests = request
        .delivery_ids
        .iter()
        .map(|delivery_id| {
            PolicyRequest::new(
                workspace_id,
                subject.clone(),
                ACTION_MESSAGE_ACK,
                PolicyResource::new(RESOURCE_MESSAGE_DELIVERY, delivery_id),
            )
        })
        .collect::<Vec<_>>();
    let authorization = policy
        .authorize_batch(&policy_requests)
        .await
        .map_err(|denial| ApiFailure::from(denial.error))?;
    Ok(messages
        .acknowledge_with_policy_revision(
            workspace_id,
            &principal,
            &request,
            authorization.revision,
        )
        .await?)
}

async fn import_core_messages(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(messages): Extension<MessageStore>,
    machine: Option<Extension<MachineSession>>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ImportMessagesRequest>,
) -> Result<Json<treer_protocol::ImportMessagesResponse>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?
    .ok_or_else(|| {
        ApiFailure::forbidden(
            "message_import_denied",
            "message import requires a local operator",
        )
    })?;
    let PolicySubject::Machine { server_id } = &subject else {
        return Err(ApiFailure::forbidden(
            "message_import_denied",
            "message import requires a local operator",
        ));
    };
    authorize_control(
        &policy,
        &workspace_id,
        Some(&subject),
        ACTION_MESSAGE_IMPORT,
        PolicyResource::new(RESOURCE_MESSAGE_IMPORT, &workspace_id),
    )
    .await?;
    let importer = MessagePrincipal {
        kind: MessagePrincipalKind::Machine,
        id: server_id.clone(),
        name: server_id.clone(),
        role: None,
    };
    Ok(Json(
        messages
            .import_legacy_mail(&workspace_id, &importer, &request)
            .await?,
    ))
}

async fn message_request_principal(
    state: &AppState,
    machine: &MachineSession,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<(PolicySubject, MessagePrincipal), ApiFailure> {
    let subject = agent_policy_subject(state, machine, headers, workspace_id).await?;
    let PolicySubject::Agent { agent_id, .. } = &subject else {
        return Err(ApiFailure::unauthorized(
            "message_agent_required",
            "managed Agent identity is required for this Message operation",
        ));
    };
    let agent = state.resolve_agent(workspace_id, agent_id).await?;
    Ok((
        subject,
        MessagePrincipal {
            kind: MessagePrincipalKind::Agent,
            id: agent.agent_id,
            name: agent.name,
            role: None,
        },
    ))
}

fn message_mailbox_policy_resource(principal: &MessagePrincipal) -> PolicyResource {
    PolicyResource::new(RESOURCE_MESSAGE_MAILBOX, &principal.id)
        .with_attribute("principal_kind", principal.kind.as_str())
}

fn message_recipient_unavailable() -> ApiFailure {
    ApiFailure::not_found(
        "message_recipient_unavailable",
        "a recipient does not exist or is not available to this sender",
    )
}

async fn agent_issue_identity_token(
    State(state): State<AppState>,
    Extension(identity_api): Extension<WorkloadIdentityApi>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<WorkloadIdentityTokenRequest>,
) -> Result<Response, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = identity_api
        .auth
        .resolve_machine_service(&workspace_id, request.audience.trim())
        .await?;
    identity_api
        .policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject.clone(),
            ACTION_IDENTITY_TOKEN_ISSUE,
            machine_service_policy_resource(
                &service.service_id,
                &service.name,
                &service.server_id,
                service.target_agent_id.as_deref(),
                &service.target_host,
                service.target_port,
            ),
        ))
        .await?;
    let PolicySubject::Agent {
        server_id,
        agent_id,
    } = subject
    else {
        return Err(ApiFailure::internal(
            "identity_subject_error",
            "identity token subject was not an agent",
        ));
    };
    let token = identity_api
        .issuer
        .issue(&workspace_id, &server_id, &agent_id, &service.service_id)
        .map_err(|error| {
            tracing::error!(%error, "failed to sign workload identity token");
            ApiFailure::internal("identity_signing_failed", "failed to sign identity token")
        })?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(token)).into_response())
}

async fn agent_list_humans(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let (subject, _) = message_request_principal(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_HUMAN_LIST,
            PolicyResource::new(RESOURCE_HUMAN_DIRECTORY, &workspace_id),
        ))
        .await?;
    Ok(Json(json!({
        "humans": auth.list_workspace_humans(&workspace_id).await?
    })))
}

async fn bootstrap_info(
    State(state): State<AppState>,
    Extension(config): Extension<BootstrapConfig>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    state.snapshot(&workspace_id).await?;
    let enrollment = auth
        .create_machine_enrollment(&workspace_id, &session.user_id)
        .await?;
    let (install_command, connect_command) = bootstrap_commands(&config.public_url, &enrollment);
    let script_url = install_script_url(&config.public_url);
    Ok(Json(json!({
        "install_command": install_command,
        "connect_command": connect_command,
        "enrollment_key": enrollment,
        "script_url": script_url.as_str(),
        "workspace_id": workspace_id,
    })))
}

fn bootstrap_commands(public_url: &Url, enrollment_key: &str) -> (String, String) {
    let script_url = install_script_url(public_url);
    let install_command = format!("curl -fsSL {} | sh", shell_quote(script_url.as_str()));
    let connect_command = format!(
        "TREER_ENROLLMENT_KEY={} treer-agent-server connect --proxy {}",
        shell_quote(enrollment_key),
        shell_quote(public_url.as_str()),
    );
    (install_command, connect_command)
}

async fn install_script(Extension(config): Extension<BootstrapConfig>) -> Response {
    let script = render_install_script(&config.public_url);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        script,
    )
        .into_response()
}

async fn enroll_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
    request: Option<Json<MachineEnrollmentRequest>>,
) -> Result<Response, ApiFailure> {
    let request = request.as_ref().map(|request| &request.0);
    let enrollment = auth
        .claim_machine_enrollment_from_headers(
            &headers,
            request.map(|request| request.installation_id.as_str()),
            request.map(|request| request.name.as_str()),
            request.and_then(|request| request.existing_server_id.as_deref()),
        )
        .await?;
    state
        .allow_server_reenrollment(&enrollment.workspace_id, &enrollment.server_id)
        .await;
    let response = MachineEnrollmentResponse {
        workspace_id: enrollment.workspace_id,
        server_id: enrollment.server_id,
        machine_token: enrollment.machine_token,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(response)).into_response())
}

async fn bind_machine_identity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(machine): Extension<MachineSession>,
    Json(request): Json<MachineEnrollmentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let workspace_id = machine.workspace_id.as_deref().ok_or_else(|| {
        ProtocolError::new(
            "machine_identity_required",
            "machine workspace identity is required",
        )
    })?;
    let server_id = machine.server_id.as_deref().ok_or_else(|| {
        ProtocolError::new(
            "machine_identity_required",
            "machine server identity is required",
        )
    })?;
    auth.bind_machine_identity(
        workspace_id,
        server_id,
        &request.installation_id,
        &request.name,
    )
    .await?;
    if state.resolve_server(workspace_id, server_id).await.is_ok() {
        let name = normalize_display_name(request.name)?;
        state.rename_server(workspace_id, server_id, name).await?;
    }
    Ok(Json(json!({ "bound": true, "server_id": server_id })))
}

async fn download_artifact(
    Extension(config): Extension<BootstrapConfig>,
    Path((platform, binary)): Path<(String, String)>,
) -> Result<Response, ApiFailure> {
    if !valid_artifact_component(&platform)
        || !matches!(
            binary.as_str(),
            "treer" | "treer-agent-host" | "treer-agent-server"
        )
    {
        return Err(ApiFailure::not_found(
            "artifact_not_found",
            "artifact not found",
        ));
    }
    let path = config.artifacts_dir.join(&platform).join(&binary);
    match tokio::fs::read(&path).await {
        Ok(data) => Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            data,
        )
            .into_response()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let release_url = release_artifact_url(&config, &platform, &binary)?;
            tracing::info!(
                path = %path.display(),
                release = %release_url,
                "redirecting missing bootstrap artifact to release"
            );
            Ok(Redirect::temporary(release_url.as_str()).into_response())
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "bootstrap artifact unavailable");
            Err(ApiFailure::not_found(
                "artifact_not_found",
                "artifact not found",
            ))
        }
    }
}

fn release_artifact_url(
    config: &BootstrapConfig,
    platform: &str,
    binary: &str,
) -> Result<Url, ApiFailure> {
    config
        .release_artifact_base_url
        .join(&format!("{binary}-{platform}"))
        .map_err(|error| {
            ProtocolError::new(
                "artifact_url_error",
                format!("failed to build release artifact URL: {error}"),
            )
            .into()
        })
}

fn install_script_url(public_url: &Url) -> Url {
    let mut url = public_url.clone();
    url.set_path("/install.sh");
    url.set_query(None);
    url
}

fn valid_artifact_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn render_install_script(public_url: &Url) -> String {
    let mut artifact_base = public_url.clone();
    artifact_base.set_path("/artifacts/");
    format!(
        r#"#!/bin/sh
set -eu

artifact_base={artifact_base}
install_dir=${{TREER_INSTALL_DIR:-"${{HOME:?HOME is required}}/.local/bin"}}
server_dir=${{TREER_AGENT_SERVER_INSTALL_DIR:-"${{HOME}}/.local/libexec/treer"}}

echo "treer: security notice" >&2
echo "treer: the Agent Server is a persistent proxy and agent host that runs with your user account's system permissions" >&2
echo "treer: workspace agents can execute commands and make network requests on this machine" >&2
echo "treer: use a dedicated account, VM, container, or other sandbox when possible" >&2

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) platform=linux-aarch64 ;;
  Darwin-x86_64|Darwin-amd64) platform=darwin-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) platform=darwin-aarch64 ;;
  *) echo "treer: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

case "$platform" in
  linux-*)
    if ! command -v unshare >/dev/null 2>&1; then
      echo "treer: warning: transparent agent networking requires unshare(1) from util-linux" >&2
    fi
    ;;
esac

if command -v curl >/dev/null 2>&1; then
  fetch() {{ curl -fsSL "$1" -o "$2"; }}
elif command -v wget >/dev/null 2>&1; then
  fetch() {{ wget -q "$1" -O "$2"; }}
else
  echo "treer: curl or wget is required" >&2
  exit 1
fi

mkdir -p "$install_dir" "$server_dir"
tmp_dir=$(mktemp -d "${{TMPDIR:-/tmp}}/treer-install.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

echo "treer: downloading $platform binaries"
fetch "$artifact_base/$platform/treer" "$tmp_dir/treer"
fetch "$artifact_base/$platform/treer-agent-host" "$tmp_dir/treer-agent-host"
fetch "$artifact_base/$platform/treer-agent-server" "$tmp_dir/treer-agent-server"
chmod 755 "$tmp_dir/treer" "$tmp_dir/treer-agent-host" "$tmp_dir/treer-agent-server"
mv "$tmp_dir/treer" "$install_dir/treer"
mv "$tmp_dir/treer-agent-host" "$server_dir/treer-agent-host"
mv "$tmp_dir/treer-agent-server" "$server_dir/treer-agent-server"
ln -sf "$server_dir/treer-agent-server" "$install_dir/treer-agent-server"

echo "treer: binaries installed"
echo "treer: add $install_dir to PATH to use treer and treer-agent-server"
echo "treer: run the workspace connection command from the Proxy UI next"
"#,
        artifact_base = shell_quote(artifact_base.as_str().trim_end_matches('/')),
    )
}

#[derive(Deserialize)]
struct ListWorkspacesQuery {
    organization_id: String,
}

#[derive(Deserialize)]
struct CreateWorkspaceApiRequest {
    organization_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    name: String,
}

async fn list_workspaces(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Query(query): Query<ListWorkspacesQuery>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "workspaces": auth
            .list_workspaces(&query.organization_id, &session.user_id)
            .await?
    })))
}

async fn create_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Json(request): Json<CreateWorkspaceApiRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let workspace_id = request
        .workspace_id
        .unwrap_or_else(|| format!("ws_{}", Uuid::new_v4().simple()));
    let info = auth
        .create_workspace(
            &request.organization_id,
            &workspace_id,
            &request.name,
            &session.user_id,
        )
        .await?;
    state.create_workspace_info(info.clone()).await?;
    Ok(Json(json!({ "workspace": info })))
}

async fn rename_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let info = auth
        .rename_workspace(&workspace_id, &session.user_id, &request.name)
        .await?;
    state.rename_workspace_info(info.clone()).await?;
    Ok(Json(json!({ "workspace": info })))
}

async fn delete_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let deleted = auth
        .delete_workspace(&workspace_id, &session.user_id)
        .await?;
    state.delete_workspace(&workspace_id).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(serde_json::to_value(&deleted)?))
}

async fn workspace_snapshot(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let mut snapshot = visible_workspace_snapshot(state.snapshot(&workspace_id).await?);
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    if let Some(PolicySubject::Machine { server_id }) = subject.as_ref() {
        snapshot
            .servers
            .retain(|server| &server.server_id == server_id);
        snapshot
            .agents
            .retain(|agent| &agent.server_id == server_id);
    } else if subject.is_some() {
        let mut visible = Vec::new();
        for agent in snapshot.agents {
            match authorize_control(
                &policy,
                &workspace_id,
                subject.as_ref(),
                ACTION_AGENT_DISCOVER,
                agent_policy_resource(&agent),
            )
            .await
            {
                Ok(()) => visible.push(agent),
                Err(error) if error.error.code == "policy_denied" => {}
                Err(error) => return Err(error),
            }
        }
        snapshot.agents = visible;
    }
    Ok(Json(serde_json::to_value(snapshot)?))
}

fn visible_workspace_snapshot(mut snapshot: WorkspaceSnapshot) -> WorkspaceSnapshot {
    snapshot.agents.retain(|agent| agent.kind != "app");
    snapshot
}

fn is_internal_app_agent_event(event: &WorkspaceEvent) -> bool {
    event.event.starts_with("agent.")
        && event.data.get("kind").and_then(Value::as_str) == Some("app")
}

async fn list_servers(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let snapshot = state.snapshot(&workspace_id).await?;
    Ok(Json(json!({ "servers": snapshot.servers })))
}

#[derive(Debug, Deserialize)]
struct MachineTrafficQuery {
    #[serde(default = "default_traffic_hours")]
    hours: u16,
}

const fn default_traffic_hours() -> u16 {
    24
}

async fn list_machine_traffic(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    Query(query): Query<MachineTrafficQuery>,
) -> Result<Json<Value>, ApiFailure> {
    if !(1..=24 * 30).contains(&query.hours) {
        return Err(ApiFailure::bad_request(
            "invalid_traffic_window",
            "traffic window must be between 1 and 720 hours",
        ));
    }
    let traffic = state
        .recent_machine_traffic(&workspace_id, query.hours)
        .await
        .map_err(|error| ApiFailure::internal("traffic_query_failed", &format!("{error:#}")))?;
    Ok(Json(json!({
        "hours": query.hours,
        "traffic": traffic,
    })))
}

async fn hydrate_app_deployment(state: &AppState, app: &mut AppDeployment) {
    if app.desired_state == AppDesiredState::Stopped {
        app.status = AppDeploymentStatus::Stopped;
        return;
    }
    if let Some(runtime_agent_id) = app.runtime_agent_id.as_deref() {
        if let Ok(agent) = state
            .resolve_agent(&app.workspace_id, runtime_agent_id)
            .await
        {
            app.pid = agent.pid;
            app.exit_code = agent.exit_code;
            app.status = if agent.status.is_terminal() {
                AppDeploymentStatus::Exited
            } else {
                AppDeploymentStatus::Running
            };
            return;
        }
    }
    app.status = if app.last_error.is_some() {
        AppDeploymentStatus::Unavailable
    } else {
        match state
            .resolve_server(&app.workspace_id, &app.server_id)
            .await
        {
            Ok(server) if server.status == ServerStatus::Online => AppDeploymentStatus::Pending,
            _ => AppDeploymentStatus::Unavailable,
        }
    };
}

fn attach_app_public_url(
    config: &IngressConfig,
    ingresses: &[ServiceIngress],
    app: &mut AppDeployment,
) {
    let managed_hostname = config.base_domain_if_configured().and_then(|base_domain| {
        managed_app_ingress_hostname(&app.name, &app.app_id, base_domain).ok()
    });
    app.public_url = ingresses
        .iter()
        .find(|ingress| {
            managed_hostname.as_deref() == Some(ingress.hostname.as_str())
                && ingress.service_id == app.service_id
                && ingress.access == ServiceIngressAccess::Workspace
                && ingress.enabled
        })
        .and_then(|ingress| config.url_for_hostname(&ingress.hostname).ok())
        .map(|url| url.to_string());
}

async fn hydrate_app_public_url(
    auth: &AuthStore,
    config: &IngressConfig,
    app: &mut AppDeployment,
) -> Result<(), ApiFailure> {
    let ingresses = auth.list_service_ingresses(&app.workspace_id).await?;
    attach_app_public_url(config, &ingresses, app);
    Ok(())
}

async fn list_app_deployments(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let mut apps = auth.list_app_deployments(&workspace_id).await?;
    let ingresses = auth.list_service_ingresses(&workspace_id).await?;
    for app in &mut apps {
        hydrate_app_deployment(&state, app).await;
        attach_app_public_url(&config, &ingresses, app);
    }
    Ok(Json(json!({ "apps": apps })))
}

async fn get_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let mut app = auth.resolve_app_deployment(&workspace_id, &target).await?;
    hydrate_app_deployment(&state, &mut app).await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

fn app_mutation_actor<'a>(
    session: Option<&'a CurrentSession>,
    subject: Option<&'a PolicySubject>,
) -> &'a str {
    session.map_or_else(
        || match subject {
            Some(PolicySubject::Agent { agent_id, .. }) => agent_id.as_str(),
            Some(PolicySubject::Machine { server_id }) => server_id.as_str(),
            Some(PolicySubject::Human { user_id }) => user_id.as_str(),
            Some(PolicySubject::Service { service_id }) => service_id.as_str(),
            None => "system",
        },
        |session| session.user_id.as_str(),
    )
}

async fn record_app_audit(
    auth: &AuthStore,
    session: Option<&CurrentSession>,
    subject: Option<&PolicySubject>,
    action: &'static str,
    app: &AppDeployment,
) {
    let (actor_kind, actor_id) = control_audit_actor(session, subject);
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &app.workspace_id,
            actor_kind,
            actor_id,
            action,
            resource_kind: "app",
            resource_id: &app.app_id,
            resource_name: Some(&app.name),
            payload: json!({
                "server_id": &app.server_id,
                "service_id": &app.service_id,
                "hostname": &app.hostname,
            }),
        })
        .await
    {
        tracing::warn!(?error, app_id = %app.app_id, action, "failed to record App audit event");
    }
}

async fn launch_app_runtime(
    state: &AppState,
    auth: &AuthStore,
    app: &AppDeployment,
    actor: &str,
) -> Result<AppDeployment, ApiFailure> {
    let runtime_agent_id = format!("appw_{}", Uuid::new_v4().simple());
    let Some(claimed) = auth
        .claim_app_runtime(
            &app.workspace_id,
            &app.app_id,
            app.runtime_agent_id.as_deref(),
            &runtime_agent_id,
            actor,
        )
        .await?
    else {
        return auth
            .resolve_app_deployment(&app.workspace_id, &app.app_id)
            .await
            .map_err(Into::into);
    };
    let workload_credential = auth
        .create_agent_credential(&app.workspace_id, &app.server_id, &runtime_agent_id)
        .await?;
    let mut args = Vec::with_capacity(app.args.len() + 1);
    args.push(app.command.clone());
    args.extend(app.args.clone());
    let result = state
        .send_command(
            &app.workspace_id,
            &app.server_id,
            AgentCommand::Create {
                agent_id: runtime_agent_id.clone(),
                workload_credential,
                request: CreateAgentRequest {
                    server_id: Some(app.server_id.clone()),
                    kind: "app".to_string(),
                    name: format!("app:{}", app.name),
                    cwd: app.cwd.clone(),
                    args,
                    cols: 120,
                    rows: 36,
                    publish_ports: vec![app.port],
                    recipe: None,
                },
            },
        )
        .await;
    if let Err(error) = result {
        auth.set_app_last_error(&app.workspace_id, &app.app_id, Some(&error.message))
            .await?;
        let _ = auth
            .delete_agent(&app.workspace_id, &runtime_agent_id)
            .await;
        return Err(error.into());
    }
    Ok(claimed)
}

async fn stop_app_runtime(
    state: &AppState,
    auth: &AuthStore,
    app: &AppDeployment,
) -> Result<(), ApiFailure> {
    let Some(runtime_agent_id) = app.runtime_agent_id.as_deref() else {
        return Ok(());
    };
    let agent = state
        .resolve_agent(&app.workspace_id, runtime_agent_id)
        .await
        .ok();
    if let Some(agent) = agent.as_ref() {
        if !agent.status.is_terminal() {
            state
                .send_command(
                    &app.workspace_id,
                    &app.server_id,
                    AgentCommand::Stop {
                        agent_id: runtime_agent_id.to_string(),
                    },
                )
                .await?;
        }
    }
    auth.delete_agent(&app.workspace_id, runtime_agent_id)
        .await?;
    if agent.is_some() {
        let _ = state
            .delete_agent(&app.workspace_id, runtime_agent_id)
            .await;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn create_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(mut request): Json<CreateAppDeploymentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let server_id = state
        .select_server(&workspace_id, request.server_id.as_deref())
        .await?;
    request.server_id = Some(server_id.clone());
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_CREATE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let mut app = auth
        .create_app_deployment(&workspace_id, actor, server_id, request)
        .await?;
    if let Some(base_domain) = config.base_domain_if_configured() {
        if let Err(error) = auth.ensure_app_ingress(&app, actor, base_domain).await {
            let _ = auth.delete_app_deployment(&workspace_id, &app.app_id).await;
            publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
            return Err(error.into());
        }
    }
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    match launch_app_runtime(&state, &auth, &app, actor).await {
        Ok(started) => app = started,
        Err(error) => {
            tracing::warn!(?error, app_id = %app.app_id, "App deployment created but its runtime did not start");
            app = auth
                .resolve_app_deployment(&workspace_id, &app.app_id)
                .await?;
        }
    }
    hydrate_app_deployment(&state, &mut app).await;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.created",
        &app,
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[allow(clippy::too_many_arguments)]
async fn start_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_CREATE,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let current = auth
        .set_app_desired_state(
            &workspace_id,
            &current.app_id,
            AppDesiredState::Running,
            actor,
        )
        .await?;
    let running = if let Some(runtime_agent_id) = current.runtime_agent_id.as_deref() {
        state
            .resolve_agent(&workspace_id, runtime_agent_id)
            .await
            .is_ok_and(|agent| !agent.status.is_terminal())
    } else {
        false
    };
    let mut app = if running {
        current
    } else {
        launch_app_runtime(&state, &auth, &current, actor).await?
    };
    hydrate_app_deployment(&state, &mut app).await;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.started",
        &app,
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[allow(clippy::too_many_arguments)]
async fn stop_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STOP,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let mut app = auth
        .set_app_desired_state(
            &workspace_id,
            &current.app_id,
            AppDesiredState::Stopped,
            actor,
        )
        .await?;
    stop_app_runtime(&state, &auth, &app).await?;
    hydrate_app_deployment(&state, &mut app).await;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.stopped",
        &app,
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[allow(clippy::too_many_arguments)]
async fn restart_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STOP,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let current = auth
        .set_app_desired_state(
            &workspace_id,
            &current.app_id,
            AppDesiredState::Running,
            actor,
        )
        .await?;
    stop_app_runtime(&state, &auth, &current).await?;
    let mut app = launch_app_runtime(&state, &auth, &current, actor).await?;
    hydrate_app_deployment(&state, &mut app).await;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.restarted",
        &app,
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[allow(clippy::too_many_arguments)]
async fn delete_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let mut response_app = current.clone();
    hydrate_app_public_url(&auth, &config, &mut response_app).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_DELETE,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    stop_app_runtime(&state, &auth, &current).await?;
    let app = auth
        .delete_app_deployment(&workspace_id, &current.app_id)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.deleted",
        &app,
    )
    .await;
    response_app.status = app.status;
    Ok(Json(json!({ "app": response_app })))
}

pub(crate) async fn reconcile_app_deployments_for_server(
    state: AppState,
    auth: AuthStore,
    workspace_id: String,
    server_id: String,
) {
    let apps = match auth
        .list_app_deployments_for_server(&workspace_id, &server_id)
        .await
    {
        Ok(apps) => apps,
        Err(error) => {
            tracing::warn!(?error, %workspace_id, %server_id, "failed to load App deployments for reconciliation");
            return;
        }
    };
    for app in apps {
        let runtime = if let Some(runtime_agent_id) = app.runtime_agent_id.as_deref() {
            state
                .resolve_agent(&workspace_id, runtime_agent_id)
                .await
                .ok()
        } else {
            None
        };
        if app.desired_state == AppDesiredState::Stopped {
            if runtime
                .as_ref()
                .is_some_and(|agent| !agent.status.is_terminal())
            {
                if let Err(error) = stop_app_runtime(&state, &auth, &app).await {
                    tracing::warn!(?error, app_id = %app.app_id, "failed to stop an undesired App runtime");
                }
            }
            continue;
        }
        if runtime
            .as_ref()
            .is_some_and(|agent| !agent.status.is_terminal())
        {
            continue;
        }
        let pending = runtime.is_none()
            && app.runtime_agent_id.is_some()
            && Utc::now().signed_duration_since(app.updated_at) < chrono::Duration::seconds(15);
        if pending {
            continue;
        }
        if app.runtime_agent_id.is_some() {
            if let Err(error) = stop_app_runtime(&state, &auth, &app).await {
                tracing::warn!(?error, app_id = %app.app_id, "failed to clean up the previous App runtime");
                continue;
            }
        }
        if let Err(error) = launch_app_runtime(&state, &auth, &app, "reconciler").await {
            tracing::warn!(?error, app_id = %app.app_id, "failed to reconcile App runtime");
        }
    }
}

async fn list_machine_services(
    Extension(auth): Extension<AuthStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "services": auth.list_machine_services(&workspace_id).await?
    })))
}

async fn create_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(mut request): Json<CreateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if let Some(target) = request.target_agent_id.as_deref() {
        let agent = state.resolve_agent(&workspace_id, target).await?;
        request.target_agent_id = Some(agent.agent_id);
        request.server_id = agent.server_id;
    } else {
        request.server_id = state
            .resolve_server(&workspace_id, &request.server_id)
            .await?
            .server_id;
    }
    let service = auth
        .create_machine_service(&workspace_id, &session.user_id, request)
        .await?;
    Ok(Json(json!({ "service": service })))
}

async fn update_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, service_id)): Path<(String, String)>,
    Json(mut request): Json<UpdateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if let Some(server_id) = request.server_id.as_deref() {
        request.server_id = Some(
            state
                .resolve_server(&workspace_id, server_id)
                .await?
                .server_id,
        );
    }
    let service = auth
        .update_machine_service(&workspace_id, &service_id, &session.user_id, request)
        .await?;
    refresh_service_ingress_routes(&auth).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "service": service })))
}

async fn delete_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    let service = auth
        .delete_machine_service(&workspace_id, &current.service_id)
        .await?;
    refresh_service_ingress_routes(&auth).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({
        "deleted": true,
        "service_id": service.service_id,
        "name": service.name,
    })))
}

async fn probe_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    let result = state
        .send_command(
            &workspace_id,
            &service.server_id,
            AgentCommand::ProbeNetwork {
                host: service.target_host.clone(),
                port: service.target_port,
                timeout_ms: 3_000,
                target_agent_id: service.target_agent_id.clone(),
            },
        )
        .await?;
    Ok(Json(json!({ "service": service, "health": result })))
}

async fn list_virtual_network_hosts(
    Extension(auth): Extension<AuthStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "hosts": auth.list_virtual_network_hosts(&workspace_id).await?
    })))
}

async fn create_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateVirtualNetworkHostRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let host = auth
        .create_virtual_network_host(&workspace_id, &session.user_id, request)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "host": host })))
}

async fn delete_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, hostname)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    auth.delete_virtual_network_host(&workspace_id, &hostname)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "deleted": true })))
}

async fn list_service_ingresses(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let ingresses = auth
        .list_service_ingresses(&workspace_id)
        .await?
        .into_iter()
        .map(|ingress| service_ingress_json(&config, ingress))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({ "ingresses": ingresses })))
}

async fn create_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateServiceIngressRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let ingress = auth
        .create_service_ingress(
            &workspace_id,
            &session.user_id,
            config.base_domain()?,
            request,
        )
        .await?;
    Ok(Json(json!({
        "ingress": service_ingress_json(&config, ingress)?
    })))
}

async fn update_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, ingress_id)): Path<(String, String)>,
    Json(request): Json<UpdateServiceIngressRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let ingress = auth
        .update_service_ingress(&workspace_id, &ingress_id, &session.user_id, request)
        .await?;
    Ok(Json(json!({
        "ingress": service_ingress_json(&config, ingress)?
    })))
}

async fn delete_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, ingress_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let ingress = auth
        .delete_service_ingress(&workspace_id, &ingress_id)
        .await?;
    Ok(Json(json!({
        "deleted": true,
        "ingress_id": ingress.ingress_id,
        "hostname": ingress.hostname,
    })))
}

fn service_ingress_json(
    config: &IngressConfig,
    ingress: ServiceIngress,
) -> Result<Value, ApiFailure> {
    let public_url = config.url_for_hostname(&ingress.hostname)?.to_string();
    let mut value = serde_json::to_value(ingress)
        .map_err(|error| ApiFailure::internal("serialization_error", &error.to_string()))?;
    value
        .as_object_mut()
        .expect("service ingress serializes as an object")
        .insert("url".to_string(), Value::String(public_url));
    Ok(value)
}

#[derive(Debug, Deserialize)]
struct IngressAuthorizeQuery {
    hostname: String,
    #[serde(default = "default_ingress_return_path")]
    return_path: String,
}

fn default_ingress_return_path() -> String {
    "/".to_string()
}

async fn authorize_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    headers: HeaderMap,
    Query(query): Query<IngressAuthorizeQuery>,
) -> Result<Response, ApiFailure> {
    let request_host = request_hostname(&headers)?;
    let proxy_host = config
        .proxy_public_url
        .host_str()
        .ok_or_else(|| ApiFailure::internal("proxy_url_error", "proxy public URL has no host"))?;
    if !request_host.eq_ignore_ascii_case(proxy_host) {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    let resolved = auth
        .resolve_service_ingress_hostname(&query.hostname)
        .await?
        .filter(|resolved| resolved.ingress.enabled)
        .ok_or_else(|| ApiFailure::not_found("ingress_not_found", "service ingress not found"))?;
    if resolved.ingress.access != ServiceIngressAccess::Workspace {
        return Ok(Redirect::to(
            config
                .url_for_hostname(&resolved.ingress.hostname)?
                .as_str(),
        )
        .into_response());
    }
    let session = match auth::authenticate_request(&auth, &headers).await {
        Ok(session) => session,
        Err(error) => {
            let (status, error) = error.into_parts();
            if status != StatusCode::UNAUTHORIZED {
                return Err(ApiFailure { status, error });
            }
            let mut authorize_url = config.proxy_public_url.clone();
            authorize_url.set_path("/.treer/ingress/authorize");
            authorize_url.set_query(None);
            authorize_url
                .query_pairs_mut()
                .append_pair("hostname", &resolved.ingress.hostname)
                .append_pair("return_path", &query.return_path);
            let mut login_url = config.app_public_url.clone();
            login_url.set_query(None);
            login_url
                .query_pairs_mut()
                .append_pair("return_to", authorize_url.as_str());
            return Ok(Redirect::to(login_url.as_str()).into_response());
        }
    };
    let code = auth
        .create_ingress_auth_code(&resolved.ingress, &session.user_id, &query.return_path)
        .await?;
    let mut callback = config.url_for_hostname(&resolved.ingress.hostname)?;
    callback.set_path("/.treer/callback");
    callback.set_query(None);
    callback.query_pairs_mut().append_pair("code", &code);
    Ok(Redirect::to(callback.as_str()).into_response())
}

async fn proxy_service_ingress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(identity): Extension<IdentityIssuer>,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let hostname = request_hostname(request.headers())?;
    if !config.matches_hostname(&hostname) {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    let resolved = auth
        .resolve_service_ingress_hostname(&hostname)
        .await?
        .filter(|resolved| resolved.ingress.enabled)
        .ok_or_else(|| ApiFailure::not_found("ingress_not_found", "service ingress not found"))?;
    if request.uri().path() == "/.treer/callback" {
        return complete_ingress_authorization(&auth, &config, &hostname, request.uri()).await;
    }
    if request.uri().path().starts_with("/.treer/") {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }

    let mut identity_token = None;
    if resolved.ingress.access == ServiceIngressAccess::Workspace && !auth.authentication_disabled()
    {
        if let Some(token) = ingress_bearer_token(request.headers())? {
            let verified = identity.verify(token, &resolved.service.service_id);
            let valid = verified.claims.as_ref().is_some_and(|claims| {
                claims.workspace_id == resolved.ingress.workspace_id
                    && claims.service_id == resolved.service.service_id
            });
            if !verified.active || !valid {
                return Err(ApiFailure::unauthorized(
                    "ingress_authentication_required",
                    "valid workspace Agent credentials are required",
                ));
            }
            identity_token = Some(token.to_string());
        } else {
            let session = cookie_value(request.headers(), config.ingress_cookie_name());
            let authenticated = match session {
                Some(token) => auth
                    .authenticate_ingress_session(&hostname, &token)
                    .await?
                    .is_some(),
                None => false,
            };
            if !authenticated {
                return redirect_to_ingress_authorization(&config, &hostname, request.uri());
            }
        }
    }

    let upgraded = request.headers().contains_key(header::UPGRADE);
    sanitize_ingress_request_headers(
        request.headers_mut(),
        upgraded,
        &hostname,
        config.public_url.as_ref().map_or("http", url::Url::scheme),
        config.ingress_cookie_name(),
        identity_token.as_deref(),
    )?;
    tunnel_http_request(
        state,
        &resolved.ingress.workspace_id,
        &resolved.service.server_id,
        resolved.service.target_agent_id.as_deref(),
        &resolved.service.target_host,
        resolved.service.target_port,
        request,
        false,
        "service ingress",
        TrafficClass::ServiceIngress,
    )
    .await
}

async fn complete_ingress_authorization(
    auth: &AuthStore,
    config: &IngressConfig,
    hostname: &str,
    uri: &Uri,
) -> Result<Response, ApiFailure> {
    let code = uri
        .query()
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
        })
        .ok_or_else(|| {
            ApiFailure::bad_request(
                "invalid_ingress_authorization",
                "authorization code missing",
            )
        })?;
    let authorization = auth.consume_ingress_auth_code(hostname, &code).await?;
    let secure = if config
        .public_url
        .as_ref()
        .is_some_and(|url| url.scheme() == "https")
    {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        config.ingress_cookie_name(),
        authorization.session_token,
        12 * 60 * 60,
        secure,
    );
    let mut response = Redirect::to(&authorization.return_path).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|error| ApiFailure::internal("cookie_error", &error.to_string()))?,
    );
    Ok(response)
}

fn redirect_to_ingress_authorization(
    config: &IngressConfig,
    hostname: &str,
    uri: &Uri,
) -> Result<Response, ApiFailure> {
    let mut url = config.proxy_public_url.clone();
    url.set_path("/.treer/ingress/authorize");
    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("hostname", hostname)
        .append_pair("return_path", &uri.to_string());
    Ok(Redirect::to(url.as_str()).into_response())
}

fn request_hostname(headers: &HeaderMap) -> Result<String, ApiFailure> {
    let authority = headers
        .get(header::HOST)
        .ok_or_else(|| ApiFailure::bad_request("host_required", "Host header is required"))?
        .to_str()
        .map_err(|_| ApiFailure::bad_request("invalid_host", "Host header is invalid"))?
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| ApiFailure::bad_request("invalid_host", "Host header is invalid"))?;
    Ok(authority.host().trim_end_matches('.').to_ascii_lowercase())
}

fn ingress_bearer_token(headers: &HeaderMap) -> Result<Option<&str>, ApiFailure> {
    let Some(value) = headers.get(TREER_AUTHORIZATION_HEADER) else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| {
        ApiFailure::unauthorized(
            "ingress_authentication_required",
            "Treer-Authorization header is invalid",
        )
    })?;
    let (scheme, token) = value.split_once(' ').ok_or_else(|| {
        ApiFailure::unauthorized(
            "ingress_authentication_required",
            "Treer-Authorization must contain a Bearer token",
        )
    })?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
        return Err(ApiFailure::unauthorized(
            "ingress_authentication_required",
            "Treer-Authorization must contain a Bearer token",
        ));
    }
    Ok(Some(token.trim()))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_string()))
}

async fn proxy_virtual_network_host_root(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, hostname)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, String::new(), request).await
}

async fn proxy_virtual_network_host_path(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, hostname, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, path, request).await
}

async fn proxy_virtual_network_host(
    state: AppState,
    auth: AuthStore,
    workspace_id: String,
    hostname: String,
    path: String,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let host = auth
        .resolve_virtual_network_host(&workspace_id, &hostname)
        .await?
        .ok_or_else(|| ApiFailure::not_found("virtual_host_not_found", &hostname))?;
    if host.service_protocol != treer_protocol::MachineServiceProtocol::Http {
        return Err(ApiFailure::bad_request(
            "service_protocol_mismatch",
            "browser access requires an HTTP service",
        ));
    }
    let query = request.uri().query();
    let target = if path.is_empty() {
        query.map_or_else(|| "/".to_string(), |query| format!("/?{query}"))
    } else {
        query.map_or_else(|| format!("/{path}"), |query| format!("/{path}?{query}"))
    };
    *request.uri_mut() = target
        .parse::<Uri>()
        .map_err(|error| ApiFailure::bad_gateway("invalid_tunnel_uri", &error.to_string()))?;
    *request.version_mut() = Version::HTTP_11;

    let upgraded = request.headers().contains_key(header::UPGRADE);
    sanitize_tunnel_request_headers(request.headers_mut(), upgraded, &host.hostname)?;
    tunnel_http_request(
        state,
        &workspace_id,
        &host.destination_server_id,
        host.destination_agent_id.as_deref(),
        &host.target_host,
        host.target_port.unwrap_or(80),
        request,
        true,
        "virtual host",
        TrafficClass::VirtualHost,
    )
    .await
}

async fn proxy_agent_interface_ui_root(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_agent_interface_ui(state, workspace_id, agent_id, String::new(), request).await
}

async fn proxy_agent_interface_ui_path(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, agent_id, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_agent_interface_ui(state, workspace_id, agent_id, path, request).await
}

async fn proxy_agent_interface_ui(
    state: AppState,
    workspace_id: String,
    target: String,
    path: String,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let interface = agent
        .interface
        .as_ref()
        .ok_or_else(|| ApiFailure::not_found("agent_interface_not_found", &agent.agent_id))?;
    let ui_path = interface
        .ui_path
        .as_deref()
        .ok_or_else(|| ApiFailure::not_found("agent_interface_ui_unavailable", &agent.agent_id))?;

    let target_path = if path.is_empty() {
        ui_path.to_string()
    } else if ui_path == "/" {
        format!("/{path}")
    } else {
        format!("{}/{path}", ui_path.trim_end_matches('/'))
    };
    let target = request.uri().query().map_or(target_path.clone(), |query| {
        format!("{target_path}?{query}")
    });
    *request.uri_mut() = target
        .parse::<Uri>()
        .map_err(|error| ApiFailure::bad_gateway("invalid_tunnel_uri", &error.to_string()))?;
    *request.version_mut() = Version::HTTP_11;
    let upgraded = request.headers().contains_key(header::UPGRADE);
    let authority = format!("127.0.0.1:{}", interface.port);
    sanitize_tunnel_request_headers(request.headers_mut(), upgraded, &authority)?;
    tunnel_http_request(
        state,
        &workspace_id,
        &agent.server_id,
        Some(&agent.agent_id),
        "127.0.0.1",
        interface.port,
        request,
        true,
        "Agent Interface UI",
        TrafficClass::AgentInterface,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn tunnel_http_request(
    state: AppState,
    workspace_id: &str,
    server_id: &str,
    target_agent_id: Option<&str>,
    target_host: &str,
    target_port: u16,
    mut request: Request<Body>,
    strip_response_cookies: bool,
    route_kind: &'static str,
    traffic_class: TrafficClass,
) -> Result<Response, ApiFailure> {
    let stream = state
        .open_browser_network_stream(
            workspace_id,
            server_id,
            target_agent_id,
            target_host,
            target_port,
            traffic_class,
        )
        .await?;
    let upgraded = request.headers().contains_key(header::UPGRADE);
    let downstream_upgrade = upgraded.then(|| hyper::upgrade::on(&mut request));
    let io = TokioIo::new(stream);
    let (mut sender, connection) = hyper::client::conn::http1::handshake::<_, Body>(io)
        .await
        .map_err(|error| ApiFailure::bad_gateway("tunnel_handshake_failed", &error.to_string()))?;
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            tracing::debug!(%error, route_kind, "HTTP tunnel connection closed");
        }
    });
    let mut response = sender
        .send_request(request)
        .await
        .map_err(|error| ApiFailure::bad_gateway("tunnel_request_failed", &error.to_string()))?;
    let target_upgrade = (response.status() == StatusCode::SWITCHING_PROTOCOLS)
        .then(|| hyper::upgrade::on(&mut response));
    if strip_response_cookies {
        sanitize_tunnel_response_headers(response.headers_mut(), target_upgrade.is_some());
    } else if target_upgrade.is_none() {
        remove_hop_by_hop_headers(response.headers_mut());
    }
    if let (Some(downstream), Some(target)) = (downstream_upgrade, target_upgrade) {
        tokio::spawn(async move {
            let Ok(downstream) = downstream.await else {
                return;
            };
            let Ok(target) = target.await else { return };
            let mut downstream = TokioIo::new(downstream);
            let mut target = TokioIo::new(target);
            let _ = tokio::io::copy_bidirectional(&mut downstream, &mut target).await;
        });
    }
    let (parts, body) = response.into_parts();
    Ok(Response::from_parts(parts, Body::new(body)))
}

fn sanitize_tunnel_request_headers(
    headers: &mut HeaderMap,
    upgraded: bool,
    hostname: &str,
) -> Result<(), ApiFailure> {
    headers.remove(header::COOKIE);
    headers.remove(header::AUTHORIZATION);
    headers.remove(header::PROXY_AUTHORIZATION);
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
    headers.insert(
        header::HOST,
        HeaderValue::from_str(hostname)
            .map_err(|error| ApiFailure::bad_gateway("invalid_virtual_host", &error.to_string()))?,
    );
    Ok(())
}

fn sanitize_ingress_request_headers(
    headers: &mut HeaderMap,
    upgraded: bool,
    hostname: &str,
    public_scheme: &str,
    ingress_cookie_name: &str,
    identity_token: Option<&str>,
) -> Result<(), ApiFailure> {
    headers.remove(header::PROXY_AUTHORIZATION);
    headers.remove(TREER_AUTHORIZATION_HEADER);
    for name in headers
        .keys()
        .filter(|name| name.as_str().starts_with("x-treer-"))
        .cloned()
        .collect::<Vec<_>>()
    {
        headers.remove(name);
    }
    if let Some(cookie) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        let forwarded = cookie
            .split(';')
            .map(str::trim)
            .filter(|item| {
                item.split_once('=')
                    .is_none_or(|(name, _)| name != ingress_cookie_name)
            })
            .collect::<Vec<_>>()
            .join("; ");
        if forwarded.is_empty() {
            headers.remove(header::COOKIE);
        } else {
            headers.insert(
                header::COOKIE,
                HeaderValue::from_str(&forwarded).map_err(|error| {
                    ApiFailure::bad_request("invalid_cookie", &error.to_string())
                })?,
            );
        }
    }
    for name in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
    ] {
        headers.remove(name);
    }
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
    headers.insert(
        header::HOST,
        HeaderValue::from_str(hostname)
            .map_err(|error| ApiFailure::bad_gateway("invalid_ingress_host", &error.to_string()))?,
    );
    headers.insert(
        "x-forwarded-host",
        HeaderValue::from_str(hostname)
            .map_err(|error| ApiFailure::bad_request("invalid_host", &error.to_string()))?,
    );
    headers.insert(
        "x-forwarded-proto",
        HeaderValue::from_str(public_scheme)
            .map_err(|error| ApiFailure::internal("invalid_ingress_scheme", &error.to_string()))?,
    );
    if let Some(token) = identity_token {
        headers.insert(
            TREER_IDENTITY_TOKEN_HEADER,
            HeaderValue::from_str(token).map_err(|error| {
                ApiFailure::bad_request("invalid_identity_token", &error.to_string())
            })?,
        );
    }
    Ok(())
}

fn sanitize_tunnel_response_headers(headers: &mut HeaderMap, upgraded: bool) {
    headers.remove(header::SET_COOKIE);
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
}

fn remove_hop_by_hop_headers(headers: &mut HeaderMap) {
    for name in [
        header::CONNECTION,
        header::UPGRADE,
        header::TRANSFER_ENCODING,
        header::TE,
        header::TRAILER,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
}

async fn agent_network_publication_forbidden() -> ApiFailure {
    ApiFailure::forbidden(
        "managed_app_required",
        "Agents cannot manage services, virtual hosts, or ingresses; deploy an App or use the operator control plane",
    )
}

async fn agent_list_machine_services(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_SERVICE_LIST,
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, "*"),
        ))
        .await?;
    list_machine_services(Extension(auth), Path(workspace_id)).await
}

async fn agent_probe_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    require_agent_can_probe_service(&subject, &service)?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_SERVICE_PROBE,
            machine_service_policy_resource(
                &service.service_id,
                &service.name,
                &service.server_id,
                service.target_agent_id.as_deref(),
                &service.target_host,
                service.target_port,
            ),
        ))
        .await?;
    let result = state
        .send_command(
            &workspace_id,
            &service.server_id,
            AgentCommand::ProbeNetwork {
                host: service.target_host.clone(),
                port: service.target_port,
                timeout_ms: 3_000,
                target_agent_id: service.target_agent_id.clone(),
            },
        )
        .await?;
    Ok(Json(json!({ "service": service, "health": result })))
}

async fn agent_list_virtual_network_hosts(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_VIRTUAL_HOST_LIST,
            PolicyResource::new(RESOURCE_VIRTUAL_HOST, "*"),
        ))
        .await?;
    list_virtual_network_hosts(Extension(auth), Path(workspace_id)).await
}

async fn agent_list_service_ingresses(
    State(state): State<AppState>,
    Extension(api): Extension<ServiceIngressApi>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    api.policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_INGRESS_LIST,
            PolicyResource::new(RESOURCE_SERVICE_INGRESS, "*"),
        ))
        .await?;
    list_service_ingresses(
        Extension(api.auth),
        Extension(api.config),
        Path(workspace_id),
    )
    .await
}

async fn publish_virtual_network_hosts(
    state: &AppState,
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<(), ApiFailure> {
    let snapshot = virtual_network_hosts_snapshot(auth, workspace_id).await?;
    state
        .broadcast_proxy_message(
            workspace_id,
            &treer_protocol::ProxyMessage::VirtualNetworkHosts { snapshot },
        )
        .await;
    Ok(())
}

async fn refresh_service_ingress_routes(auth: &AuthStore) -> Result<(), ApiFailure> {
    auth.refresh_service_ingresses()
        .await
        .map_err(|error| ApiFailure::internal("ingress_refresh_failed", &format!("{error:#}")))
}

pub fn spawn_network_metadata_refresh(state: AppState, auth: AuthStore) {
    tokio::spawn(async move {
        let mut refresh = tokio::time::interval(std::time::Duration::from_secs(5));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        refresh.tick().await;
        loop {
            refresh.tick().await;
            if let Err(error) = auth.refresh_virtual_network_hosts().await {
                tracing::warn!(%error, "failed to reload virtual hosts");
                continue;
            }
            if let Err(error) = auth.refresh_service_ingresses().await {
                tracing::warn!(%error, "failed to reload service ingresses");
            }
            let Ok(workspaces) = auth.all_workspaces().await else {
                tracing::warn!("failed to list workspaces for virtual-host refresh");
                continue;
            };
            for workspace in workspaces {
                match virtual_network_hosts_snapshot(&auth, &workspace.workspace_id).await {
                    Ok(snapshot) => {
                        state
                            .broadcast_proxy_message(
                                &workspace.workspace_id,
                                &treer_protocol::ProxyMessage::VirtualNetworkHosts { snapshot },
                            )
                            .await;
                    }
                    Err(error) => {
                        let (_, error) = error.into_parts();
                        tracing::warn!(
                            workspace = %workspace.workspace_id,
                            message = %error.message,
                            "failed to refresh virtual hosts"
                        );
                    }
                }
            }
        }
    });
}

pub(crate) async fn virtual_network_hosts_snapshot(
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<VirtualNetworkHostsSnapshot, auth::AuthFailure> {
    auth.virtual_network_hosts_snapshot(workspace_id).await
}

async fn agent_policy_subject(
    state: &AppState,
    machine: &MachineSession,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<PolicySubject, ApiFailure> {
    let agent_id = headers
        .get(AGENT_ID_HEADER)
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .map_err(|_| ProtocolError::new("invalid_agent_identity", "agent ID is invalid"))
        })
        .transpose()?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ProtocolError::new(
                "invalid_agent_identity",
                "managed agent identity is required",
            )
        })?;
    let agent = state.resolve_agent(workspace_id, agent_id).await?;
    if machine
        .server_id
        .as_ref()
        .is_some_and(|server_id| server_id != &agent.server_id)
    {
        return Err(ProtocolError::new(
            "policy_subject_mismatch",
            "agent does not belong to the authenticated machine",
        )
        .into());
    }
    Ok(PolicySubject::Agent {
        server_id: agent.server_id,
        agent_id: agent.agent_id,
    })
}

async fn control_policy_subject(
    state: &AppState,
    machine: Option<&MachineSession>,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<Option<PolicySubject>, ApiFailure> {
    let Some(machine) = machine else {
        return Ok(None);
    };
    if headers.contains_key(AGENT_ID_HEADER) {
        agent_policy_subject(state, machine, headers, workspace_id)
            .await
            .map(Some)
    } else {
        Ok(Some(PolicySubject::Machine {
            server_id: machine.server_id.clone().ok_or_else(|| {
                ProtocolError::new("machine_identity_required", "machine identity is required")
            })?,
        }))
    }
}

fn require_machine_target(
    subject: Option<&PolicySubject>,
    target_server_id: &str,
) -> Result<(), ApiFailure> {
    if let Some(PolicySubject::Machine { server_id }) = subject {
        if server_id != target_server_id {
            return Err(ProtocolError::new(
                "agent_identity_required",
                "cross-machine operations require an authenticated Agent workload credential",
            )
            .into());
        }
    }
    Ok(())
}

async fn authorize_control(
    policy: &PolicyEngine,
    workspace_id: &str,
    subject: Option<&PolicySubject>,
    action: &str,
    resource: PolicyResource,
) -> Result<(), ApiFailure> {
    let Some(subject) = subject else {
        return Ok(());
    };
    policy
        .authorize(&PolicyRequest::new(
            workspace_id,
            subject.clone(),
            action,
            resource,
        ))
        .await?;
    Ok(())
}

fn agent_policy_resource(agent: &AgentInfo) -> PolicyResource {
    PolicyResource::new(RESOURCE_AGENT, &agent.agent_id)
        .with_attribute("server_id", &agent.server_id)
}

fn policy_actor_name(subject: &PolicySubject) -> String {
    match subject {
        PolicySubject::Agent { agent_id, .. } => format!("agent:{agent_id}"),
        PolicySubject::Machine { server_id } => format!("machine:{server_id}"),
        PolicySubject::Human { user_id } => format!("human:{user_id}"),
        PolicySubject::Service { service_id } => format!("service:{service_id}"),
    }
}

fn launch_profile_policy_resource(profile_id: &str, name: &str) -> PolicyResource {
    PolicyResource::new(RESOURCE_AGENT_LAUNCH_PROFILE, profile_id).with_attribute("name", name)
}

fn profile_actor_label(
    session: Option<&CurrentSession>,
    subject: Option<&PolicySubject>,
) -> String {
    session.map_or_else(
        || {
            subject
                .map(policy_actor_name)
                .unwrap_or_else(|| "system".to_string())
        },
        |session| session.user_id.clone(),
    )
}

async fn prompt_installer_recipe(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
    agent_id: &str,
    recipe: &str,
) -> Result<(), ProtocolError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let output = state
            .send_command(
                workspace_id,
                server_id,
                AgentCommand::Read {
                    agent_id: agent_id.to_string(),
                    lines: Some(80),
                },
            )
            .await?;
        let text = output
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if text.contains("Do you trust") || text.contains("Press enter to continue") {
            state
                .send_command(
                    workspace_id,
                    server_id,
                    AgentCommand::Input {
                        agent_id: agent_id.to_string(),
                        data: vec![b'\r'],
                    },
                )
                .await?;
        } else if installer_composer_ready(text) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    state
        .send_command(
            workspace_id,
            server_id,
            AgentCommand::Prompt {
                agent_id: agent_id.to_string(),
                text: installer_base_prompt(recipe),
            },
        )
        .await?;
    Ok(())
}

fn queue_installer_recipe_prompt(
    state: AppState,
    workspace_id: String,
    server_id: String,
    agent_id: String,
    recipe: String,
) {
    tokio::spawn(async move {
        if let Err(error) =
            prompt_installer_recipe(&state, &workspace_id, &server_id, &agent_id, &recipe).await
        {
            tracing::warn!(
                ?error,
                %workspace_id,
                %agent_id,
                "failed to prompt installer with bundled skill"
            );
        }
    });
}

fn require_agent_can_probe_service(
    subject: &PolicySubject,
    service: &MachineService,
) -> Result<(), ApiFailure> {
    match subject {
        PolicySubject::Agent { server_id, .. } if server_id == &service.server_id => Ok(()),
        PolicySubject::Agent { .. } => Err(ApiFailure::forbidden(
            "service_not_owned",
            "agents may probe only services on their own machine",
        )),
        PolicySubject::Machine { .. }
        | PolicySubject::Human { .. }
        | PolicySubject::Service { .. } => Err(ApiFailure::forbidden(
            "ingress_agent_required",
            "a managed agent identity is required to probe a service",
        )),
    }
}

fn machine_service_policy_resource(
    service_id: &str,
    name: &str,
    server_id: &str,
    target_agent_id: Option<&str>,
    target_host: &str,
    target_port: u16,
) -> PolicyResource {
    let resource = PolicyResource::new(RESOURCE_MACHINE_SERVICE, service_id)
        .with_attribute("name", name)
        .with_attribute("server_id", server_id)
        .with_attribute("target_host", target_host)
        .with_attribute("target_port", target_port.to_string());
    if let Some(agent_id) = target_agent_id {
        resource.with_attribute("target_agent_id", agent_id)
    } else {
        resource
    }
}

async fn list_agent_launch_profiles(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_LIST,
        PolicyResource::new(RESOURCE_AGENT_LAUNCH_PROFILE, "*"),
    )
    .await?;
    Ok(Json(json!({
        "profiles": auth.list_agent_launch_profiles(&workspace_id).await?
    })))
}

async fn get_agent_launch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let profile = auth
        .resolve_agent_launch_profile(&workspace_id, &target)
        .await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_READ,
        launch_profile_policy_resource(&profile.profile_id, &profile.name),
    )
    .await?;
    Ok(Json(serde_json::to_value(profile)?))
}

#[allow(clippy::too_many_arguments)]
async fn create_agent_launch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateAgentLaunchProfileRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_CREATE,
        launch_profile_policy_resource("new", request.name.trim()),
    )
    .await?;
    let actor_label = profile_actor_label(session.as_deref(), subject.as_ref());
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    let profile = auth
        .create_agent_launch_profile(
            &workspace_id,
            ProfileMutationActor {
                kind: actor_kind,
                id: actor_id,
                label: &actor_label,
            },
            request,
        )
        .await?;
    Ok(Json(serde_json::to_value(profile)?))
}

#[allow(clippy::too_many_arguments)]
async fn update_agent_launch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<UpdateAgentLaunchProfileRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let profile = auth
        .resolve_agent_launch_profile(&workspace_id, &target)
        .await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_UPDATE,
        launch_profile_policy_resource(&profile.profile_id, &profile.name),
    )
    .await?;
    let actor_label = profile_actor_label(session.as_deref(), subject.as_ref());
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    let profile = auth
        .update_agent_launch_profile(
            &workspace_id,
            &profile.profile_id,
            ProfileMutationActor {
                kind: actor_kind,
                id: actor_id,
                label: &actor_label,
            },
            request,
        )
        .await?;
    Ok(Json(serde_json::to_value(profile)?))
}

#[allow(clippy::too_many_arguments)]
async fn delete_agent_launch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let profile = auth
        .resolve_agent_launch_profile(&workspace_id, &target)
        .await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_DELETE,
        launch_profile_policy_resource(&profile.profile_id, &profile.name),
    )
    .await?;
    let actor_label = profile_actor_label(session.as_deref(), subject.as_ref());
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    let profile = auth
        .delete_agent_launch_profile(
            &workspace_id,
            &profile.profile_id,
            ProfileMutationActor {
                kind: actor_kind,
                id: actor_id,
                label: &actor_label,
            },
        )
        .await?;
    Ok(Json(serde_json::to_value(profile)?))
}

#[allow(clippy::too_many_arguments)]
async fn launch_agent_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<LaunchAgentProfileRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let profile = auth
        .resolve_agent_launch_profile(&workspace_id, &target)
        .await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_USE,
        launch_profile_policy_resource(&profile.profile_id, &profile.name),
    )
    .await?;
    let profile_id = profile.profile_id.clone();
    let agent_request = agent_request_from_launch_profile(&profile, request)?;
    let data = execute_agent_create(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        subject.as_ref(),
        &workspace_id,
        agent_request,
        Some(&profile_id),
    )
    .await?;
    Ok(Json(data))
}

fn agent_request_from_launch_profile(
    profile: &AgentLaunchProfile,
    request: LaunchAgentProfileRequest,
) -> Result<CreateAgentRequest, ProtocolError> {
    let agent_name = request
        .agent_name
        .map(normalize_display_name)
        .transpose()?
        .unwrap_or_else(|| profile.name.clone());
    let mut args = Vec::with_capacity(profile.args.len() + 1);
    args.push(profile.command.clone());
    args.extend(profile.args.clone());
    Ok(CreateAgentRequest {
        server_id: request.server_id,
        kind: "shell".to_string(),
        name: agent_name,
        cwd: request.cwd.unwrap_or_else(|| profile.cwd.clone()),
        args,
        cols: request.cols,
        rows: request.rows,
        publish_ports: Vec::new(),
        recipe: None,
    })
}

async fn list_agents(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let snapshot = state.snapshot(&workspace_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let mut agents = Vec::new();
    for agent in snapshot.agents {
        if agent.kind == "app" {
            continue;
        }
        if matches!(subject.as_ref(), Some(PolicySubject::Machine { server_id }) if server_id != &agent.server_id)
        {
            continue;
        }
        match authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION_AGENT_DISCOVER,
            agent_policy_resource(&agent),
        )
        .await
        {
            Ok(()) => agents.push(agent),
            Err(error) if error.error.code == "policy_denied" => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Json(json!({ "agents": agents })))
}

async fn get_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_METADATA_READ,
        agent_policy_resource(&agent),
    )
    .await?;
    Ok(Json(serde_json::to_value(agent)?))
}

#[allow(clippy::too_many_arguments)]
async fn rename_server(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_UPDATE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let name = normalize_display_name(request.name)?;
    auth.set_machine_name(&workspace_id, &server_id, &name)
        .await?;
    let renamed = state
        .rename_server(&workspace_id, &server_id, name.clone())
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.renamed",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: Some(&name),
            payload: json!({}),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record runtime audit event");
    }
    Ok(Json(serde_json::to_value(renamed)?))
}

async fn delete_server(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let server = state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_DELETE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let agents = state
        .snapshot(&workspace_id)
        .await?
        .agents
        .into_iter()
        .filter(|agent| agent.server_id == server_id)
        .collect::<Vec<_>>();
    let stops = agents
        .iter()
        .filter(|agent| !agent.status.is_terminal())
        .map(|agent| async {
            tokio::time::timeout(
                Duration::from_secs(3),
                state.send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::Stop {
                        agent_id: agent.agent_id.clone(),
                    },
                ),
            )
            .await
        });
    let _ = futures_util::future::join_all(stops).await;
    let shutdown_requested = if server.labels.get("treer.shutdown").map(String::as_str) == Some("1")
    {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                state.send_command(&workspace_id, &server_id, AgentCommand::ShutdownMachine),
            )
            .await,
            Ok(Ok(_))
        )
    } else {
        false
    };
    let agent_ids = agents
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    auth.delete_machine(&workspace_id, &server_id, &agent_ids)
        .await?;
    let (server, deleted_agents) = state.delete_server(&workspace_id, &server_id).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.deleted",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: Some(&server.name),
            payload: json!({ "deleted_agent_count": deleted_agents.len(), "shutdown_requested": shutdown_requested }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record runtime audit event");
    }
    Ok(Json(json!({
        "server": server,
        "deleted_agents": deleted_agents,
        "shutdown_requested": shutdown_requested,
    })))
}

#[allow(clippy::too_many_arguments)]
async fn rename_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_UPDATE,
        agent_policy_resource(&agent),
    )
    .await?;
    let name = normalize_display_name(request.name)?;
    auth.set_agent_name(&workspace_id, &agent.agent_id, &name)
        .await?;
    let renamed = state
        .rename_agent(&workspace_id, &agent.agent_id, name.clone())
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "agent.renamed",
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, agent_id = %agent.agent_id, "failed to record runtime audit event");
    }
    Ok(Json(serde_json::to_value(renamed)?))
}

async fn delete_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_DELETE,
        agent_policy_resource(&agent),
    )
    .await?;
    if !agent.status.is_terminal() {
        state
            .send_command(
                &workspace_id,
                &agent.server_id,
                AgentCommand::Stop {
                    agent_id: agent.agent_id.clone(),
                },
            )
            .await?;
    }
    auth.delete_agent(&workspace_id, &agent.agent_id).await?;
    auth.refresh_virtual_network_hosts()
        .await
        .map_err(|error| {
            ApiFailure::internal("virtual_host_refresh_failed", &format!("{error:#}"))
        })?;
    refresh_service_ingress_routes(&auth).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    let deleted = state.delete_agent(&workspace_id, &agent.agent_id).await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "agent.deleted",
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&agent.name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, agent_id = %agent.agent_id, "failed to record runtime audit event");
    }
    Ok(Json(serde_json::to_value(deleted)?))
}

fn normalize_display_name(name: String) -> Result<String, ProtocolError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ProtocolError::new(
            "invalid_name",
            "name must contain 1-80 visible characters",
        ));
    }
    Ok(name.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn create_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let data = execute_agent_create(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        subject.as_ref(),
        &workspace_id,
        request,
        None,
    )
    .await?;
    Ok(Json(data))
}

#[allow(clippy::too_many_arguments)]
async fn execute_agent_create(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    session: Option<&CurrentSession>,
    subject: Option<&PolicySubject>,
    workspace_id: &str,
    request: CreateAgentRequest,
    launch_profile_id: Option<&str>,
) -> Result<Value, ApiFailure> {
    if let Some(recipe) = recipe_url(&request) {
        if let Err(message) = validate_recipe_url(recipe) {
            return Err(ApiFailure::bad_request("invalid_recipe", &message));
        }
        if !recipe_installer_kind_allowed(&request.kind) {
            return Err(ApiFailure::bad_request(
                "recipe_requires_interactive_agent",
                "recipe install requires an interactive agent kind or auto",
            ));
        }
    }
    let recipe = recipe_url(&request).map(str::to_string);
    let server_id = state
        .select_server(workspace_id, request.server_id.as_deref())
        .await?;
    require_machine_target(subject, &server_id)?;
    authorize_control(
        policy,
        workspace_id,
        subject,
        ACTION_AGENT_CREATE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    if let Some(recipe) = recipe.as_deref() {
        if let Ok(snapshot) = state.snapshot(workspace_id).await {
            let filter = if request.kind == "auto" {
                None
            } else {
                Some(request.kind.as_str())
            };
            if let Some(existing) =
                pick_existing_installer_agent(&snapshot.agents, &server_id, filter)
            {
                let agent_id = existing.agent_id.clone();
                let mut data = serde_json::to_value(existing).unwrap_or_else(|_| json!({}));
                queue_installer_recipe_prompt(
                    state.clone(),
                    workspace_id.to_string(),
                    server_id.clone(),
                    agent_id,
                    recipe.to_string(),
                );
                if let Some(object) = data.as_object_mut() {
                    object.insert("installer_reused".into(), json!(true));
                    object.insert("installer_prompted".into(), json!("queued"));
                }
                return Ok(data);
            }
        }
    }
    let agent_id = format!("ag_{}", Uuid::new_v4().simple());
    let agent_name = request.name.clone();
    let workload_credential = auth
        .create_agent_credential(workspace_id, &server_id, &agent_id)
        .await?;
    let mut data = state
        .send_command(
            workspace_id,
            &server_id,
            AgentCommand::Create {
                agent_id: agent_id.clone(),
                workload_credential,
                request,
            },
        )
        .await?;
    if let Some(recipe) = recipe.as_deref() {
        queue_installer_recipe_prompt(
            state.clone(),
            workspace_id.to_string(),
            server_id.clone(),
            agent_id.clone(),
            recipe.to_string(),
        );
        if let Some(object) = data.as_object_mut() {
            object.insert("installer_prompted".into(), json!("queued"));
        }
    }
    let (actor_kind, actor_id) = control_audit_actor(session, subject);
    let payload = launch_profile_id.map_or_else(
        || json!({ "server_id": &server_id }),
        |profile_id| json!({ "server_id": &server_id, "launch_profile_id": profile_id }),
    );
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id,
            actor_kind,
            actor_id,
            action: "agent.created",
            resource_kind: "agent",
            resource_id: &agent_id,
            resource_name: Some(&agent_name),
            payload,
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %agent_id, "failed to record runtime audit event");
    }
    Ok(data)
}

async fn prompt_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<PromptAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_PROMPT,
        agent_policy_resource(&agent),
    )
    .await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Prompt {
                agent_id: agent.agent_id,
                text: request.text,
            },
        )
        .await?;
    Ok(Json(data))
}

async fn input_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<InputAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_INPUT,
        agent_policy_resource(&agent),
    )
    .await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Input {
                agent_id: agent.agent_id,
                data: request.data,
            },
        )
        .await?;
    Ok(Json(data))
}

async fn read_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_OUTPUT_READ,
        agent_policy_resource(&agent),
    )
    .await?;
    let lines = query.get("lines").and_then(|value| value.parse().ok());
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Read {
                agent_id: agent.agent_id,
                lines,
            },
        )
        .await?;
    Ok(Json(data))
}

async fn read_agent_transcript(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_OUTPUT_READ,
        agent_policy_resource(&agent),
    )
    .await?;
    let cursor = query
        .get("page")
        .cloned()
        .or_else(|| query.get("cursor").cloned());
    let limit = query.get("limit").and_then(|value| value.parse().ok());
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Transcript {
                agent_id: agent.agent_id,
                cursor,
                limit,
            },
        )
        .await?;
    Ok(Json(data))
}

async fn stop_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STOP,
        agent_policy_resource(&agent),
    )
    .await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Stop {
                agent_id: agent.agent_id.clone(),
            },
        )
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "agent.stopped",
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&agent.name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, agent_id = %agent.agent_id, "failed to record runtime audit event");
    }
    Ok(Json(data))
}

#[derive(Debug, Deserialize)]
struct TerminalQuery {
    #[serde(default = "default_terminal_cols")]
    cols: u16,
    #[serde(default = "default_terminal_rows")]
    rows: u16,
    #[serde(default)]
    stream_epoch: Option<String>,
    #[serde(default)]
    since_revision: Option<u64>,
    #[serde(default)]
    flow_control: bool,
}

const fn default_terminal_cols() -> u16 {
    120
}

const fn default_terminal_rows() -> u16 {
    36
}

impl TerminalQuery {
    fn cursor(&self) -> Option<TerminalCursor> {
        let stream_epoch = self.stream_epoch.as_deref()?.trim();
        if stream_epoch.is_empty() {
            return None;
        }
        Some(TerminalCursor {
            stream_epoch: stream_epoch.to_string(),
            revision: self.since_revision.unwrap_or(0),
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn agent_terminal(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_INPUT,
        agent_policy_resource(&agent),
    )
    .await?;
    Ok(ws.on_upgrade(move |socket| stream_terminal(socket, state, workspace_id, agent_id, query)))
}

async fn stream_terminal(
    socket: WebSocket,
    state: AppState,
    workspace_id: String,
    agent_id: String,
    query: TerminalQuery,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let (terminal_tx, mut terminal_rx) =
        tokio::sync::mpsc::channel::<SocketFrame>(TERMINAL_BROWSER_QUEUE_CAPACITY);
    let cursor = query.cursor();
    let attached = state
        .attach_terminal(
            &workspace_id,
            &agent_id,
            query.cols,
            query.rows,
            cursor,
            terminal_tx,
        )
        .await;
    let session_id = match attached {
        Ok(session_id) => session_id,
        Err(error) => {
            let message = TerminalServerMessage::Error { error };
            if let Ok(encoded) = serde_json::to_string(&message) {
                let _ = outgoing.send(Message::Text(encoded.into())).await;
            }
            return;
        }
    };

    let flow_window_bytes = query.flow_control.then_some(TERMINAL_FLOW_WINDOW_BYTES);
    let mut in_flight_bytes = 0usize;
    loop {
        tokio::select! {
            message = incoming.next() => {
                let Some(Ok(message)) = message else { break };
                let result = match message {
                    Message::Binary(data) => state.terminal_input(&session_id, data.to_vec()).await,
                    Message::Text(text) => match serde_json::from_str::<TerminalClientMessage>(&text) {
                        Ok(TerminalClientMessage::Resize { cols, rows }) => {
                            state.terminal_resize(&session_id, cols, rows).await
                        }
                        Ok(TerminalClientMessage::Ack { bytes }) => {
                            let Some(_) = flow_window_bytes else {
                                continue;
                            };
                            let bytes = bytes as usize;
                            if bytes > in_flight_bytes {
                                Err(ProtocolError::new(
                                    "invalid_terminal_ack",
                                    "terminal acknowledgement exceeds outstanding output",
                                ))
                            } else {
                                in_flight_bytes -= bytes;
                                Ok(())
                            }
                        }
                        Err(error) => Err(ProtocolError::new("invalid_terminal_message", error.to_string())),
                    },
                    Message::Close(_) => break,
                    _ => continue,
                };
                if let Err(error) = result {
                    let message = TerminalServerMessage::Error { error };
                    if let Ok(encoded) = serde_json::to_string(&message) {
                        if outgoing.send(Message::Text(encoded.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            frame = terminal_rx.recv(), if flow_window_bytes.is_none_or(|window| in_flight_bytes < window) => {
                let Some(frame) = frame else { break };
                let binary_bytes = match &frame {
                    SocketFrame::Binary(data) => data.len(),
                    _ => 0,
                };
                let message = match frame {
                    SocketFrame::Text(encoded) => Message::Text(encoded.into()),
                    SocketFrame::Binary(data) => Message::Binary(data.into()),
                    SocketFrame::Ping(payload) => Message::Ping(payload.into()),
                    SocketFrame::Pong(payload) => Message::Pong(payload.into()),
                    SocketFrame::Close => Message::Close(None),
                };
                if outgoing.send(message).await.is_err() {
                    break;
                }
                if flow_window_bytes.is_some() {
                    in_flight_bytes = in_flight_bytes.saturating_add(binary_bytes);
                }
            }
        }
    }
    state.detach_terminal(&session_id).await;
}

async fn workspace_events(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    state.snapshot(&workspace_id).await?;
    Ok(ws.on_upgrade(move |socket| stream_workspace_events(socket, state, workspace_id)))
}

async fn stream_workspace_events(socket: WebSocket, state: AppState, workspace_id: String) {
    let (mut outgoing, mut incoming) = socket.split();
    let mut events = state.subscribe();
    if let Ok(snapshot) = state.snapshot(&workspace_id).await {
        let snapshot = visible_workspace_snapshot(snapshot);
        let initial = WorkspaceEvent {
            revision: snapshot.revision,
            workspace_id: workspace_id.clone(),
            event: "workspace.snapshot".to_string(),
            data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
        };
        if send_event(&mut outgoing, &initial).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) if event.workspace_id == workspace_id => {
                    if is_internal_app_agent_event(&event) {
                        continue;
                    }
                    if send_event(&mut outgoing, &event).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let Ok(snapshot) = state.snapshot(&workspace_id).await else { break };
                    let snapshot = visible_workspace_snapshot(snapshot);
                    let event = WorkspaceEvent {
                        revision: snapshot.revision,
                        workspace_id: workspace_id.clone(),
                        event: "workspace.snapshot".to_string(),
                        data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
                    };
                    if send_event(&mut outgoing, &event).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            message = incoming.next() => {
                if message.is_none() || message.is_some_and(|item| item.is_err()) {
                    break;
                }
            }
        }
    }
}

async fn send_event(
    outgoing: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    event: &WorkspaceEvent,
) -> Result<(), axum::Error> {
    let encoded = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    outgoing.send(Message::Text(encoded.into())).await
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
            "policy_denied" | "policy_subject_mismatch" => StatusCode::FORBIDDEN,
            "server_offline" | "no_online_server" | "ssh_unsupported" | "scp_unsupported" => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            "invalid_agent_identity" | "invalid_name" | "invalid_request" => {
                StatusCode::BAD_REQUEST
            }
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::process::{Command, Stdio};
    use tower::ServiceExt;
    use treer_protocol::{
        CommandResult, MachineServiceProtocol, PolicyEffect, PolicyMode, PolicyPrincipalKind,
        PolicyPrincipalRef, ProxyMessage, WorkspacePolicyDocument, POLICY_SCHEMA_VERSION,
    };
    use treer_proxy::policy_store::WorkspacePolicyStore;

    async fn state_with_managed_agent() -> AppState {
        let state = AppState::new();
        let now = chrono::Utc::now();
        let server = treer_protocol::ServerInfo {
            server_id: "machine-a".to_string(),
            workspace_id: "default".to_string(),
            name: "machine-a".to_string(),
            hostname: "machine-a".to_string(),
            root: "/tmp".to_string(),
            controller_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "controller-test".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "host-test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: treer_protocol::ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        };
        let agent = treer_protocol::AgentInfo {
            agent_id: "agent-a".to_string(),
            workspace_id: "default".to_string(),
            server_id: "machine-a".to_string(),
            kind: "command".to_string(),
            name: "agent-a".to_string(),
            cwd: ".".to_string(),
            status: treer_protocol::AgentStatus::Idle,
            pid: None,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
            interface: None,
        };
        let mut recipient = agent.clone();
        recipient.agent_id = "agent-b".to_string();
        recipient.name = "reviewer".to_string();
        let connection_id = Uuid::new_v4();
        let (outgoing, _incoming) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server(server.clone(), connection_id, outgoing)
            .await
            .expect("register server");
        state
            .apply_snapshot(
                connection_id,
                treer_protocol::AgentServerSnapshot {
                    server,
                    agents: vec![agent, recipient],
                },
            )
            .await
            .expect("apply agent snapshot");
        state
    }

    #[tokio::test]
    async fn public_workspace_updates_hide_app_runtime_agents() {
        let state = state_with_managed_agent().await;
        let mut app_agent = state
            .resolve_agent("default", "agent-a")
            .await
            .expect("resolve fixture agent");
        app_agent.agent_id = "appw-test".to_string();
        app_agent.name = "app:Docs".to_string();
        app_agent.kind = "app".to_string();
        state.test_insert_agent(app_agent.clone()).await;

        let snapshot = state.snapshot("default").await.expect("workspace snapshot");
        assert!(snapshot.agents.iter().any(|agent| agent.kind == "app"));

        let visible = visible_workspace_snapshot(snapshot);
        assert!(visible.agents.iter().all(|agent| agent.kind != "app"));
        assert_eq!(visible.agents.len(), 2);

        let event = WorkspaceEvent {
            revision: 1,
            workspace_id: "default".to_string(),
            event: "agent.updated".to_string(),
            data: serde_json::to_value(app_agent).expect("encode app agent"),
        };
        assert!(is_internal_app_agent_event(&event));
    }

    #[tokio::test]
    async fn app_reconcile_stops_a_runtime_left_running_while_the_machine_was_offline() {
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let app = auth
            .create_app_deployment(
                "default",
                "owner",
                "machine-a".to_string(),
                CreateAppDeploymentRequest {
                    server_id: Some("machine-a".to_string()),
                    name: "Docs".to_string(),
                    command: "python3".to_string(),
                    args: vec!["-m".to_string(), "http.server".to_string()],
                    cwd: ".".to_string(),
                    port: 8080,
                    hostname: "docs.internal".to_string(),
                },
            )
            .await
            .expect("create App");
        let runtime_id = "appw_offline_stop";
        auth.claim_app_runtime("default", &app.app_id, None, runtime_id, "reconciler")
            .await
            .expect("claim runtime")
            .expect("runtime claim");
        auth.set_app_desired_state("default", &app.app_id, AppDesiredState::Stopped, "owner")
            .await
            .expect("persist stopped state");

        let state = AppState::new();
        let now = Utc::now();
        let server = treer_protocol::ServerInfo {
            server_id: "machine-a".to_string(),
            workspace_id: "default".to_string(),
            name: "machine-a".to_string(),
            hostname: "machine-a".to_string(),
            root: "/tmp".to_string(),
            controller_build: treer_protocol::BuildInfo {
                version: "test".to_string(),
                git_commit: "test".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "test".to_string(),
                git_commit: "test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        };
        let runtime = AgentInfo {
            agent_id: runtime_id.to_string(),
            workspace_id: "default".to_string(),
            server_id: "machine-a".to_string(),
            kind: "app".to_string(),
            name: "app:Docs".to_string(),
            cwd: ".".to_string(),
            status: treer_protocol::AgentStatus::Idle,
            pid: Some(42),
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
            interface: None,
        };
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server(server.clone(), connection_id, server_tx)
            .await
            .expect("register server");
        state
            .apply_snapshot(
                connection_id,
                treer_protocol::AgentServerSnapshot {
                    server,
                    agents: vec![runtime],
                },
            )
            .await
            .expect("apply snapshot");

        let reconcile = tokio::spawn(reconcile_app_deployments_for_server(
            state.clone(),
            auth,
            "default".to_string(),
            "machine-a".to_string(),
        ));
        let command: ProxyMessage = match server_rx.recv().await.expect("stop command") {
            SocketFrame::Text(value) => serde_json::from_str(&value).expect("decode command"),
            _ => panic!("expected text command"),
        };
        let ProxyMessage::Command { envelope } = command else {
            panic!("expected command envelope");
        };
        assert!(matches!(
            envelope.command,
            AgentCommand::Stop { ref agent_id } if agent_id == runtime_id
        ));
        state
            .complete_command(CommandResult::success(envelope.command_id, json!({})))
            .await;
        reconcile.await.expect("join reconcile");
        assert!(state.resolve_agent("default", runtime_id).await.is_err());
    }

    fn test_config() -> BootstrapConfig {
        BootstrapConfig::new(
            Url::parse("https://treer.example/").expect("valid URL"),
            PathBuf::from("dist"),
            Url::parse("https://github.example/releases/latest/download")
                .expect("valid release URL"),
        )
    }

    fn test_browser_access() -> BrowserAccess {
        BrowserAccess::new(
            &Url::parse("https://app.treer.ai/").expect("app URL"),
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        )
        .expect("browser access")
    }

    #[test]
    fn browser_tunnels_accept_app_and_proxy_origins_only() {
        let access = test_browser_access();
        for origin in ["https://app.treer.ai", "https://proxy.treer.ai"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
            access
                .validate_tunnel_if_present(&headers)
                .expect("trusted tunnel origin");
        }
        let mut denied = HeaderMap::new();
        denied.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://attacker.example"),
        );
        assert!(access.validate_tunnel_if_present(&denied).is_err());
    }

    fn test_ingress_config() -> IngressConfig {
        IngressConfig::new(
            Some(Url::parse("https://apps.treer.ai/").expect("ingress URL")),
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
            &Url::parse("https://app.treer.ai/").expect("app URL"),
        )
        .expect("ingress config")
    }

    async fn admin_router(updater: crate::updater::UpdaterClient) -> Router {
        let auth = AuthStore::for_test("admin-password").await;
        let messages = MessageStore::open(auth.pool())
            .await
            .expect("message store");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        )
        .await
        .expect("identity issuer");
        router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
            test_ingress_config(),
            messages,
            CapabilityRollout::all_enabled(),
            updater,
        )
    }

    async fn admin_cookie(app: Router) -> (Router, HeaderValue) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"password":"admin-password"}"#))
                    .expect("login request"),
            )
            .await
            .expect("login response");
        assert_eq!(response.status(), StatusCode::OK);
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("admin cookie")
            .to_str()
            .expect("cookie text");
        let token = set_cookie.split(';').next().expect("cookie pair");
        (app, HeaderValue::from_str(token).expect("cookie header"))
    }

    async fn spawn_updater_sidecar() -> Url {
        let app = Router::new()
            .route(
                "/v1/status",
                get(|| async {
                    Json(serde_json::json!({"channel":"stable","services":[],"job":null}))
                }),
            )
            .route(
                "/v1/apply",
                post(|| async {
                    (
                        StatusCode::ACCEPTED,
                        Json(serde_json::json!({
                            "channel": "stable",
                            "job": {"id": "job1", "state": "running", "error": null}
                        })),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind sidecar");
        let addr = listener.local_addr().expect("sidecar address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("sidecar server");
        });
        tokio::task::yield_now().await;
        Url::parse(&format!("http://{addr}/")).expect("sidecar URL")
    }

    #[test]
    fn ingress_config_matches_one_wildcard_label_and_builds_urls() {
        let config = test_ingress_config();
        assert!(config.matches_hostname("demo.apps.treer.ai"));
        assert!(!config.matches_hostname("apps.treer.ai"));
        assert!(!config.matches_hostname("nested.demo.apps.treer.ai"));
        assert_eq!(
            config
                .url_for_hostname("demo.apps.treer.ai")
                .expect("ingress URL")
                .as_str(),
            "https://demo.apps.treer.ai/"
        );
    }

    #[tokio::test]
    async fn trailing_slash_browser_tunnel_route_is_registered() {
        let auth = AuthStore::for_test("admin-password").await;
        let messages = MessageStore::open(auth.pool())
            .await
            .expect("message store");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://treer.example/").expect("public URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
            test_ingress_config(),
            messages,
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/workspaces/default/virtual-hosts/self.test/proxy/")
                    .body(Body::empty())
                    .expect("tunnel request"),
            )
            .await
            .expect("route response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn workspace_deletion_route_requires_auth_and_deletes_the_workspace() {
        let auth = AuthStore::for_test("admin-password").await;
        let (invite, _) = auth
            .create_personal_invitation()
            .await
            .expect("personal invitation");
        let messages = MessageStore::open(auth.pool())
            .await
            .expect("message store");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://treer.example/").expect("public URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth.clone(),
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
            test_ingress_config(),
            messages,
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/auth/register")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "invite": invite,
                            "email": "owner@example.com",
                            "preferred_name": "Owner",
                            "password": "password123",
                        }))
                        .expect("register body"),
                    ))
                    .expect("register request"),
            )
            .await
            .expect("register response");
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = session_cookie(&response);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/organizations")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .expect("organization list request"),
            )
            .await
            .expect("organization list response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("organization list body");
        let payload: Value = serde_json::from_slice(&bytes).expect("organization list JSON");
        let organization_id = payload["organizations"]
            .as_array()
            .expect("organization array")
            .first()
            .expect("personal organization")
            .get("organization_id")
            .expect("organization id")
            .as_str()
            .expect("organization id text")
            .to_string();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/workspaces")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, &cookie)
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "organization_id": organization_id,
                            "name": "Deletable",
                        }))
                        .expect("create workspace body"),
                    ))
                    .expect("create workspace request"),
            )
            .await
            .expect("create workspace response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("create workspace body");
        let payload: Value = serde_json::from_slice(&bytes).expect("create workspace JSON");
        let workspace_id = payload["workspace"]["workspace_id"]
            .as_str()
            .expect("workspace id")
            .to_string();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/api/workspaces/{workspace_id}"))
                    .body(Body::empty())
                    .expect("unauthenticated delete"),
            )
            .await
            .expect("unauthenticated response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/api/workspaces/{workspace_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .expect("authenticated delete"),
            )
            .await
            .expect("delete response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("delete body");
        let payload: Value = serde_json::from_slice(&bytes).expect("delete JSON");
        assert_eq!(payload["workspace_id"], workspace_id);
        assert_eq!(payload["name"], "Deletable");
        assert_eq!(payload["organization_id"], organization_id);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/workspaces?organization_id={organization_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .expect("workspace list request"),
            )
            .await
            .expect("list response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("list body");
        let payload: Value = serde_json::from_slice(&bytes).expect("list JSON");
        assert!(payload["workspaces"]
            .as_array()
            .expect("workspace array")
            .is_empty());

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri(format!("/api/workspaces/{workspace_id}"))
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .expect("repeat delete"),
            )
            .await
            .expect("repeat delete response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    fn session_cookie(response: &axum::response::Response) -> String {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with("treer_session="))
            .map(|value| value.split(';').next().expect("cookie pair").to_string())
            .expect("session cookie")
    }

    #[tokio::test]
    async fn rollout_gates_core_message_traffic() {
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let enrollment = auth
            .create_machine_enrollment("default", "test")
            .await
            .expect("create rollout test enrollment");
        let machine = auth
            .claim_machine_enrollment(&enrollment)
            .await
            .expect("claim rollout test machine");
        let authorization = HeaderValue::from_str(&format!("Bearer {}", machine.machine_token))
            .expect("machine authorization header");
        let messages = MessageStore::open(auth.pool())
            .await
            .expect("message store");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://treer.example/").expect("public URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
            test_ingress_config(),
            messages,
            CapabilityRollout::new(false),
            crate::updater::UpdaterClient::disabled(),
        );

        for path in [
            "/agent/workspaces/default/messages",
            "/api/apps/svc_mail/messages",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(path)
                        .header(header::AUTHORIZATION, authorization.clone())
                        .body(Body::empty())
                        .expect("gated request"),
                )
                .await
                .expect("gated route response");
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read rollout error");
            let error: ApiError = serde_json::from_slice(&body).expect("decode rollout error");
            assert_eq!(error.error.code, "core_messages_disabled");
        }
    }

    #[tokio::test]
    async fn machine_routes_expose_managed_apps_but_deny_direct_network_publication() {
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let enrollment = auth
            .create_machine_enrollment("default", "test")
            .await
            .expect("create App route enrollment");
        let machine = auth
            .claim_machine_enrollment(&enrollment)
            .await
            .expect("claim App route machine");
        let messages = MessageStore::open(auth.pool())
            .await
            .expect("message store");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://treer.example/").expect("public URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
            test_ingress_config(),
            messages,
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/agent/workspaces/default/apps")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", machine.machine_token),
                    )
                    .body(Body::empty())
                    .expect("App list request"),
            )
            .await
            .expect("App list response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read App list response");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).expect("decode App list"),
            json!({ "apps": [] })
        );

        for (method, path) in [
            (Method::POST, "/agent/workspaces/default/services"),
            (Method::PATCH, "/agent/workspaces/default/services/svc_old"),
            (Method::DELETE, "/agent/workspaces/default/services/svc_old"),
            (Method::POST, "/agent/workspaces/default/virtual-hosts"),
            (
                Method::DELETE,
                "/agent/workspaces/default/virtual-hosts/old.internal",
            ),
            (Method::POST, "/agent/workspaces/default/ingresses"),
            (Method::PATCH, "/agent/workspaces/default/ingresses/ing_old"),
            (
                Method::DELETE,
                "/agent/workspaces/default/ingresses/ing_old",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header(
                            header::AUTHORIZATION,
                            format!("Bearer {}", machine.machine_token),
                        )
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .expect("network mutation request"),
                )
                .await
                .expect("network mutation response");
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read network mutation response");
            let error: ApiError =
                serde_json::from_slice(&body).expect("decode network mutation error");
            assert_eq!(error.error.code, "managed_app_required", "{path}");
        }
    }

    #[tokio::test]
    async fn workspace_ingress_redirects_humans_to_proxy_authorization() {
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let service = auth
            .create_machine_service(
                "default",
                "test-user",
                CreateMachineServiceRequest {
                    name: "private app".to_string(),
                    server_id: "machine-a".to_string(),
                    target_agent_id: None,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 8080,
                    protocol: treer_protocol::MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create service");
        let ingress = auth
            .create_service_ingress(
                "default",
                "test-user",
                "apps.treer.ai",
                CreateServiceIngressRequest {
                    service_id: service.service_id,
                    slug: Some("private".to_string()),
                    access: ServiceIngressAccess::Workspace,
                },
            )
            .await
            .expect("create ingress");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        )
        .await
        .expect("identity issuer");
        let request = Request::builder()
            .uri("/dashboard?tab=active")
            .header(header::HOST, &ingress.hostname)
            .body(Body::empty())
            .expect("ingress request");
        let response = proxy_service_ingress(
            State(AppState::new()),
            Extension(auth),
            Extension(test_ingress_config()),
            Extension(identity),
            request,
        )
        .await
        .expect("authorization redirect");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .expect("redirect location")
            .to_str()
            .expect("location text");
        assert!(location.starts_with("https://proxy.treer.ai/.treer/ingress/authorize?"));
        assert!(location.contains("hostname=private-"));
        assert!(location.contains("return_path=%2Fdashboard%3Ftab%3Dactive"));
    }

    #[test]
    fn browser_access_accepts_only_the_configured_origin_when_present() {
        let browser = test_browser_access();
        assert!(browser.validate_if_present(&HeaderMap::new()).is_ok());

        let mut allowed = HeaderMap::new();
        allowed.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://app.treer.ai"),
        );
        assert!(browser.validate_if_present(&allowed).is_ok());

        let mut denied = HeaderMap::new();
        denied.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://other.treer.ai"),
        );
        let error = browser
            .validate_if_present(&denied)
            .expect_err("other browser origin must be denied");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.error.code, "browser_origin_denied");
    }

    #[tokio::test]
    async fn cors_preflight_allows_the_configured_app_with_credentials() {
        let auth = AuthStore::for_test("admin-password").await;
        let messages = MessageStore::open(auth.pool())
            .await
            .expect("message store");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
            test_ingress_config(),
            messages,
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/auth/login")
                    .header(header::ORIGIN, "https://app.treer.ai")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                    .body(Body::empty())
                    .expect("preflight request"),
            )
            .await
            .expect("preflight response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://app.treer.ai"))
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
            Some(&HeaderValue::from_static("true"))
        );
    }

    #[tokio::test]
    async fn cors_headers_are_present_on_authenticated_route_errors() {
        let auth = AuthStore::for_test("admin-password").await;
        let messages = MessageStore::open(auth.pool())
            .await
            .expect("message store");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
            test_ingress_config(),
            messages,
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header(header::ORIGIN, "https://app.treer.ai")
                    .body(Body::empty())
                    .expect("authenticated request"),
            )
            .await
            .expect("authenticated response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://app.treer.ai"))
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
            Some(&HeaderValue::from_static("true"))
        );
    }

    #[tokio::test]
    async fn admin_update_requires_an_admin_session() {
        let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/update")
                    .body(Body::empty())
                    .expect("update request"),
            )
            .await
            .expect("update response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_update_is_unconfigured_without_a_sidecar() {
        let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
        let (app, cookie) = admin_cookie(app).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/update")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .expect("update request"),
            )
            .await
            .expect("update response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let error: ApiError = serde_json::from_slice(&body).expect("decode");
        assert_eq!(error.error.code, "updater_unconfigured");
    }

    #[tokio::test]
    async fn admin_update_forwards_to_the_sidecar() {
        let sidecar = spawn_updater_sidecar().await;
        let updater =
            crate::updater::UpdaterClient::new(sidecar, "secret".to_string()).expect("client");
        let app = admin_router(updater).await;
        let (app, cookie) = admin_cookie(app).await;
        let status = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/update")
                    .header(header::COOKIE, cookie.clone())
                    .body(Body::empty())
                    .expect("status request"),
            )
            .await
            .expect("status response");
        assert_eq!(status.status(), StatusCode::OK);
        let apply = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/update")
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("apply request"),
            )
            .await
            .expect("apply response");
        assert_eq!(apply.status(), StatusCode::ACCEPTED);
    }

    #[test]
    fn bootstrap_command_keeps_enrollment_tokens_out_of_the_url() {
        let config = test_config();
        let url = install_script_url(&config.public_url);
        assert_eq!(url.as_str(), "https://treer.example/install.sh");
        assert!(url.query().is_none());
    }

    #[tokio::test]
    async fn legacy_enrollment_requests_without_identity_remain_supported() {
        let auth = AuthStore::for_test("admin-password").await;
        let enrollment = auth
            .create_machine_enrollment("default", "admin")
            .await
            .expect("create enrollment");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {enrollment}"))
                .expect("enrollment authorization"),
        );
        let response = enroll_machine(State(AppState::new()), Extension(auth), headers, None)
            .await
            .unwrap_or_else(|error| panic!("legacy enrollment: {}", error.error.message));
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn bootstrap_separates_public_installation_from_workspace_connection() {
        let config = test_config();
        let key = "enr_v1_64656661756c74_abc.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let (install, connect) = bootstrap_commands(&config.public_url, key);
        assert_eq!(
            install,
            "curl -fsSL 'https://treer.example/install.sh' | sh"
        );
        assert!(!install.contains("enr_"));
        assert!(!install.contains("connect"));
        assert!(connect.contains(key));
        assert!(connect.contains("treer-agent-server connect --proxy"));
        assert!(!connect.contains("install.sh"));
    }

    #[test]
    fn installer_is_posix_shell_and_only_installs_binaries() {
        let config = test_config();
        let script = render_install_script(&config.public_url);
        assert!(script.starts_with("#!/bin/sh\nset -eu\n"));
        assert!(script.contains("platform=linux-aarch64"));
        assert!(script.contains("transparent agent networking requires unshare(1)"));
        assert!(script.contains("persistent proxy and agent host"));
        assert!(script.contains("container, or other sandbox"));
        assert!(script.contains(".local/libexec/treer"));
        assert!(script.contains("treer-agent-host"));
        assert!(script.contains(
            "ln -sf \"$server_dir/treer-agent-server\" \"$install_dir/treer-agent-server\""
        ));
        assert!(script.contains("https://treer.example/artifacts"));
        assert!(!script.contains("service --workspace"));
        assert!(!script.contains("machine_token"));
        assert!(!script.contains("TREER_MACHINE_TOKEN"));
        assert!(!script.contains("TREER_ENROLLMENT_KEY"));
        assert!(!script.contains("systemctl"));
        assert!(!script.contains("launchctl"));
        assert!(!script.contains("nohup"));
    }

    #[cfg(unix)]
    #[test]
    fn rendered_installer_has_valid_shell_syntax() {
        let config = test_config();
        let script = render_install_script(&config.public_url);
        let mut child = Command::new("sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .spawn()
            .expect("start shell parser");
        child
            .stdin
            .take()
            .expect("shell stdin")
            .write_all(script.as_bytes())
            .expect("write installer");
        assert!(child.wait().expect("wait for shell parser").success());
    }

    #[test]
    fn artifact_paths_reject_directory_traversal() {
        assert!(valid_artifact_component("linux-aarch64"));
        assert!(!valid_artifact_component("../linux-aarch64"));
        assert!(!valid_artifact_component("linux/aarch64"));
    }

    #[test]
    fn release_artifact_names_match_tagged_assets() {
        let config = test_config();
        let url = release_artifact_url(&config, "darwin-aarch64", "treer-agent-host")
            .unwrap_or_else(|_| panic!("release artifact URL"));
        assert_eq!(
            url.as_str(),
            "https://github.example/releases/latest/download/treer-agent-host-darwin-aarch64"
        );
    }

    #[tokio::test]
    async fn missing_local_artifacts_redirect_to_the_release() {
        let mut config = test_config();
        config.artifacts_dir = std::env::temp_dir().join(format!(
            "treer-missing-artifacts-{}",
            Uuid::new_v4().simple()
        ));
        let response = download_artifact(
            Extension(config),
            Path(("darwin-aarch64".to_string(), "treer".to_string())),
        )
        .await
        .unwrap_or_else(|_| panic!("missing artifact should redirect"));
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://github.example/releases/latest/download/treer-darwin-aarch64")
        );
    }

    #[test]
    fn display_names_are_trimmed_and_validated() {
        assert_eq!(
            normalize_display_name("  build machine  ".to_string()).expect("valid name"),
            "build machine"
        );
        assert!(normalize_display_name("  ".to_string()).is_err());
        assert!(normalize_display_name("bad\nname".to_string()).is_err());
        assert!(normalize_display_name("x".repeat(81)).is_err());
    }

    #[test]
    fn launch_profiles_become_interactive_shell_requests() {
        let timestamp = "2026-08-20T00:00:00Z".parse().expect("valid timestamp");
        let profile = AgentLaunchProfile {
            profile_id: "alp_review".to_string(),
            workspace_id: "default".to_string(),
            name: "Reviewer".to_string(),
            description: String::new(),
            cwd: "packages/api".to_string(),
            command: "codex".to_string(),
            args: vec![
                "review".to_string(),
                "--base".to_string(),
                "main".to_string(),
            ],
            created_at: timestamp,
            created_by: "user".to_string(),
            updated_at: timestamp,
            updated_by: "user".to_string(),
        };
        let request = agent_request_from_launch_profile(
            &profile,
            LaunchAgentProfileRequest {
                server_id: Some("machine-a".to_string()),
                agent_name: None,
                cwd: Some("reviews/42".to_string()),
                cols: 100,
                rows: 30,
            },
        )
        .expect("build create request");
        assert_eq!(request.server_id.as_deref(), Some("machine-a"));
        assert_eq!(request.kind, "shell");
        assert_eq!(request.name, "Reviewer");
        assert_eq!(request.cwd, "reviews/42");
        assert_eq!(request.args, ["codex", "review", "--base", "main"]);
        assert_eq!((request.cols, request.rows), (100, 30));

        let request = agent_request_from_launch_profile(
            &profile,
            LaunchAgentProfileRequest {
                server_id: None,
                agent_name: None,
                cwd: None,
                cols: 120,
                rows: 36,
            },
        )
        .expect("build request with profile cwd");
        assert_eq!(request.cwd, "packages/api");
    }

    #[test]
    fn machine_principals_cannot_target_another_machine() {
        let subject = PolicySubject::Machine {
            server_id: "machine-a".to_string(),
        };
        assert!(require_machine_target(Some(&subject), "machine-a").is_ok());
        let error = require_machine_target(Some(&subject), "machine-b")
            .expect_err("cross-machine operation must require Agent identity");
        assert_eq!(error.error.code, "agent_identity_required");
    }

    #[test]
    fn same_machine_agents_can_probe_sibling_services() {
        let timestamp = "2026-08-20T00:00:00Z".parse().expect("valid timestamp");
        let service = MachineService {
            service_id: "svc_ui".to_string(),
            workspace_id: "default".to_string(),
            name: "codex-ui".to_string(),
            server_id: "machine-a".to_string(),
            target_agent_id: Some("ag_ui".to_string()),
            target_host: "127.0.0.1".to_string(),
            target_port: 4173,
            protocol: MachineServiceProtocol::Http,
            created_at: timestamp,
            created_by: "agent:ag_ui".to_string(),
            updated_at: timestamp,
            updated_by: "agent:ag_ui".to_string(),
        };
        let installer = PolicySubject::Agent {
            server_id: "machine-a".to_string(),
            agent_id: "ag_installer".to_string(),
        };
        let other_machine = PolicySubject::Agent {
            server_id: "machine-b".to_string(),
            agent_id: "ag_other".to_string(),
        };
        assert!(require_agent_can_probe_service(&installer, &service).is_ok());
        assert_eq!(
            require_agent_can_probe_service(&other_machine, &service)
                .expect_err("cross-machine probe")
                .error
                .code,
            "service_not_owned"
        );
    }

    #[tokio::test]
    async fn agent_policy_subject_is_bound_to_authenticated_machine() {
        let state = state_with_managed_agent().await;
        let machine = MachineSession {
            server_id: Some("machine-a".to_string()),
            workspace_id: Some("default".to_string()),
        };
        let missing = agent_policy_subject(&state, &machine, &HeaderMap::new(), "default")
            .await
            .expect_err("agent identity is required");
        assert_eq!(missing.status, StatusCode::BAD_REQUEST);
        assert_eq!(missing.error.code, "invalid_agent_identity");

        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
        let subject = agent_policy_subject(&state, &machine, &headers, "default")
            .await
            .unwrap_or_else(|error| panic!("matching subject: {}", error.error.message));
        assert_eq!(
            subject,
            PolicySubject::Agent {
                server_id: "machine-a".to_string(),
                agent_id: "agent-a".to_string(),
            }
        );

        let error = agent_policy_subject(
            &state,
            &MachineSession {
                server_id: Some("machine-b".to_string()),
                workspace_id: Some("default".to_string()),
            },
            &headers,
            "default",
        )
        .await
        .expect_err("foreign machine must not claim the agent");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.error.code, "policy_subject_mismatch");
    }

    #[tokio::test]
    async fn agent_identity_tokens_use_the_canonical_service_audience() {
        let state = state_with_managed_agent().await;
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let service = auth
            .create_machine_service(
                "default",
                "test",
                CreateMachineServiceRequest {
                    name: "api".to_string(),
                    server_id: "machine-a".to_string(),
                    target_agent_id: None,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 8080,
                    protocol: treer_protocol::MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create service");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://treer.example/").expect("public URL"),
        )
        .await
        .expect("identity issuer");
        let machine = MachineSession {
            server_id: Some("machine-a".to_string()),
            workspace_id: Some("default".to_string()),
        };
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));

        let response = agent_issue_identity_token(
            State(state.clone()),
            Extension(WorkloadIdentityApi {
                auth: auth.clone(),
                policy: PolicyEngine::allow_all(),
                issuer: identity.clone(),
            }),
            Extension(machine),
            headers,
            Path("default".to_string()),
            Json(WorkloadIdentityTokenRequest {
                audience: "api".to_string(),
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("issue identity token: {}", error.error.message));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read token response");
        let token: treer_protocol::WorkloadIdentityTokenResponse =
            serde_json::from_slice(&body).expect("decode token response");
        assert_eq!(token.audience, service.service_id);
        let verified = identity.verify(&token.access_token, &service.service_id);
        assert!(verified.active);
        let claims = verified.claims.expect("verified claims");
        assert_eq!(claims.sub, "agent-a");
        assert_eq!(claims.machine_id, "machine-a");

        let mut app_headers = HeaderMap::new();
        app_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token.access_token))
                .expect("App authorization header"),
        );
        let (workspace_id, subject, principal) =
            app_message_identity(&state, &auth, &identity, &app_headers, &service.service_id)
                .await
                .expect("authenticate Agent App Message identity");
        assert_eq!(workspace_id, "default");
        assert_eq!(principal.id, "agent-a");
        assert!(matches!(
            subject,
            PolicySubject::Agent { server_id, agent_id }
                if server_id == "machine-a" && agent_id == "agent-a"
        ));
    }

    #[test]
    fn app_recipients_share_one_agent_and_human_namespace() {
        let principals = vec![
            AppPrincipal {
                kind: AppPrincipalKind::Agent,
                id: "agent-reviewer".to_string(),
                name: "reviewer".to_string(),
                role: None,
            },
            AppPrincipal {
                kind: AppPrincipalKind::Human,
                id: "user-owner".to_string(),
                name: "Owner".to_string(),
                role: Some("owner".to_string()),
            },
            AppPrincipal {
                kind: AppPrincipalKind::Human,
                id: "user-reviewer".to_string(),
                name: "reviewer".to_string(),
                role: Some("member".to_string()),
            },
        ];
        let owner = resolve_app_principal(&principals, "Owner").expect("unique human name");
        assert_eq!(owner.kind, AppPrincipalKind::Human);
        assert_eq!(owner.id, "user-owner");
        let stable = resolve_app_principal(&principals, "agent-reviewer").expect("stable Agent ID");
        assert_eq!(stable.kind, AppPrincipalKind::Agent);
        let ambiguous =
            resolve_app_principal(&principals, "reviewer").expect_err("ambiguous display name");
        assert_eq!(ambiguous.code, "recipient_ambiguous");
    }

    #[tokio::test]
    async fn message_api_pins_durable_policy_and_hides_recipient_resolution_details() {
        let state = state_with_managed_agent().await;
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO users(id, email, email_verified, preferred_name, password_hash, created_at) \
             VALUES('user-reviewer', 'reviewer@example.test', TRUE, 'reviewer', 'unused', $1)",
        )
        .bind(&now)
        .execute(&auth.pool())
        .await
        .expect("seed duplicate-name human");
        sqlx::query(
            "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             VALUES('org_default', 'user-reviewer', 'member', $1)",
        )
        .bind(&now)
        .execute(&auth.pool())
        .await
        .expect("seed duplicate-name membership");

        let messages = MessageStore::open(auth.pool())
            .await
            .expect("message store");
        let policy_store = WorkspacePolicyStore::new(auth.pool());
        let deny_send = WorkspacePolicyDocument {
            schema_version: POLICY_SCHEMA_VERSION,
            defaults: BTreeMap::from([(ACTION_MESSAGE_SEND.to_string(), PolicyEffect::Deny)]),
            groups: BTreeMap::new(),
            rules: Vec::new(),
        };
        let actor = PolicyPrincipalRef {
            kind: PolicyPrincipalKind::Human,
            id: "policy-owner".to_string(),
        };
        let monitor = policy_store
            .replace(
                "default",
                0,
                PolicyMode::Monitor,
                deny_send.clone(),
                actor.clone(),
            )
            .await
            .expect("install monitor policy");
        assert_eq!(monitor.revision, 1);

        let machine = MachineSession {
            server_id: Some("machine-a".to_string()),
            workspace_id: Some("default".to_string()),
        };
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
        let secret_body = "api-policy-body-must-not-enter-metadata";
        let request = |recipient: &str, key: &str, body: &str| SendMessageRequest {
            recipients: vec![recipient.to_string()],
            context_ids: Vec::new(),
            body: body.to_string(),
            expires_at: None,
            idempotency_key: Some(key.to_string()),
            correlation_id: Some("cor_api_policy".to_string()),
            trace_id: Some("trace_api_policy".to_string()),
            external_source: None,
        };

        let sent = send_core_message(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(PolicyEngine::durable(policy_store.clone())),
            Extension(messages.clone()),
            Extension(machine.clone()),
            Path("default".to_string()),
            headers.clone(),
            Json(request("agent-b", "api-monitor-send", secret_body)),
        )
        .await
        .expect("monitor policy must observe rather than deny");

        let envelope: Value = sqlx::query_scalar(
            "SELECT envelope FROM core_message_outbox \
             WHERE workspace_id = 'default' AND action = 'message.created' \
             ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(&auth.pool())
        .await
        .expect("load Message outbox envelope");
        assert_eq!(envelope["resource"]["id"], sent.0.message.message_id);
        assert_eq!(envelope["workspace_revision"], monitor.revision);
        assert!(!envelope.to_string().contains(secret_body));

        let audit_payloads: Vec<String> =
            sqlx::query_scalar("SELECT payload::text FROM organization_audit_events")
                .fetch_all(&auth.pool())
                .await
                .expect("load audit payloads");
        assert!(
            audit_payloads
                .iter()
                .all(|payload| !payload.contains(secret_body)),
            "Message bodies must not enter audit payloads"
        );

        let nonexistent = send_core_message(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(PolicyEngine::allow_all()),
            Extension(messages.clone()),
            Extension(machine.clone()),
            Path("default".to_string()),
            headers.clone(),
            Json(request("missing-recipient", "api-missing", "missing body")),
        )
        .await
        .expect_err("nonexistent recipient must be hidden");
        let duplicate = send_core_message(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(PolicyEngine::allow_all()),
            Extension(messages.clone()),
            Extension(machine.clone()),
            Path("default".to_string()),
            headers.clone(),
            Json(request("reviewer", "api-duplicate", "duplicate body")),
        )
        .await
        .expect_err("duplicate-name recipient must be hidden");

        let enforce = policy_store
            .replace(
                "default",
                monitor.revision,
                PolicyMode::Enforce,
                deny_send,
                actor,
            )
            .await
            .expect("enforce policy");
        assert_eq!(enforce.revision, 2);
        let hidden = send_core_message(
            State(state),
            Extension(auth.clone()),
            Extension(PolicyEngine::durable(policy_store)),
            Extension(messages),
            Extension(machine),
            Path("default".to_string()),
            headers,
            Json(request("agent-b", "api-hidden", "hidden body")),
        )
        .await
        .expect_err("enforced policy must hide the recipient");

        for error in [&nonexistent, &duplicate, &hidden] {
            assert_eq!(error.status, StatusCode::NOT_FOUND);
            assert_eq!(error.error.code, "message_recipient_unavailable");
            assert_eq!(
                error.error.message,
                "a recipient does not exist or is not available to this sender"
            );
        }
        let stored_messages: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM core_messages WHERE workspace_id = 'default'")
                .fetch_one(&auth.pool())
                .await
                .expect("count stored Messages");
        assert_eq!(stored_messages, 1, "all denied sends must be atomic");
    }

    #[tokio::test]
    async fn browser_tunnel_rejects_tcp_services_before_opening_a_stream() {
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let service = auth
            .create_machine_service(
                "default",
                "test-user",
                CreateMachineServiceRequest {
                    name: "database".to_string(),
                    server_id: "machine-a".to_string(),
                    target_agent_id: None,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 5432,
                    protocol: treer_protocol::MachineServiceProtocol::Tcp,
                },
            )
            .await
            .expect("create machine service");
        auth.create_virtual_network_host(
            "default",
            "test-user",
            CreateVirtualNetworkHostRequest {
                hostname: "database.internal".to_string(),
                service_id: service.service_id,
            },
        )
        .await
        .expect("create virtual host");

        let error = proxy_virtual_network_host(
            AppState::new(),
            auth,
            "default".to_string(),
            "database.internal".to_string(),
            String::new(),
            Request::new(Body::empty()),
        )
        .await
        .expect_err("TCP services must not enter the HTTP tunnel");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.error.code, "service_protocol_mismatch");
    }

    #[tokio::test]
    async fn browser_tunnel_forwards_http_without_leaking_gateway_credentials() {
        let state = AppState::new();
        let now = chrono::Utc::now();
        let server = treer_protocol::ServerInfo {
            server_id: "machine-a".to_string(),
            workspace_id: "default".to_string(),
            name: "machine-a".to_string(),
            hostname: "machine-a".to_string(),
            root: "/tmp".to_string(),
            controller_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "controller-test".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "host-test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: treer_protocol::ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        };
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server(server, connection_id, server_tx)
            .await
            .expect("register controller");

        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let service = auth
            .create_machine_service(
                "default",
                "test-user",
                CreateMachineServiceRequest {
                    name: "app".to_string(),
                    server_id: "machine-a".to_string(),
                    target_agent_id: None,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 8080,
                    protocol: treer_protocol::MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create machine service");
        auth.create_virtual_network_host(
            "default",
            "test-user",
            CreateVirtualNetworkHostRequest {
                hostname: "app.internal".to_string(),
                service_id: service.service_id,
            },
        )
        .await
        .expect("create virtual host");

        let controller_state = state.clone();
        let controller = tokio::spawn(async move {
            let open = match server_rx.recv().await.expect("network open") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
                }
                _ => panic!("expected network open"),
            };
            assert_eq!(open.kind, treer_protocol::NetworkBinaryKind::Open);
            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Opened,
                        stream_id: open.stream_id.clone(),
                        payload: Vec::new(),
                    },
                )
                .await
                .expect("open stream");

            let request = loop {
                let frame = match server_rx.recv().await.expect("HTTP request frame") {
                    SocketFrame::Binary(encoded) => {
                        treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                    }
                    _ => continue,
                };
                if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                    break String::from_utf8(frame.payload).expect("HTTP request text");
                }
            };
            assert!(request.starts_with("GET /status?full=1 HTTP/1.1\r\n"));
            assert!(request.contains("host: app.internal\r\n"));
            assert!(!request.to_ascii_lowercase().contains("cookie:"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));

            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\nSet-Cookie: internal=secret\r\n\r\nhello".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::HalfClose,
                        stream_id: open.stream_id,
                        payload: Vec::new(),
                    },
                )
                .await
                .expect("close response");
        });

        let request = Request::builder()
            .uri("/ignored?full=1")
            .header(header::COOKIE, "treer_session=secret")
            .header(header::AUTHORIZATION, "Bearer secret")
            .body(Body::empty())
            .expect("browser request");
        let response = proxy_virtual_network_host(
            state,
            auth,
            "default".to_string(),
            "app.internal".to_string(),
            "status".to_string(),
            request,
        )
        .await
        .unwrap_or_else(|error| panic!("tunnel request: {}", error.error.message));
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("read tunnel response");
        assert_eq!(&body[..], b"hello");
        controller.await.expect("join controller");
    }

    #[tokio::test]
    async fn public_ingress_preserves_application_auth_and_strips_treer_headers() {
        let state = AppState::new();
        let now = chrono::Utc::now();
        let server = treer_protocol::ServerInfo {
            server_id: "machine-a".to_string(),
            workspace_id: "default".to_string(),
            name: "machine-a".to_string(),
            hostname: "machine-a".to_string(),
            root: "/tmp".to_string(),
            controller_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "controller-test".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "host-test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: treer_protocol::ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        };
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server(server, connection_id, server_tx)
            .await
            .expect("register controller");

        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let service = auth
            .create_machine_service(
                "default",
                "test-user",
                CreateMachineServiceRequest {
                    name: "public app".to_string(),
                    server_id: "machine-a".to_string(),
                    target_agent_id: None,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 8080,
                    protocol: treer_protocol::MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create machine service");
        let ingress = auth
            .create_service_ingress(
                "default",
                "test-user",
                "apps.treer.ai",
                CreateServiceIngressRequest {
                    service_id: service.service_id,
                    slug: Some("demo".to_string()),
                    access: ServiceIngressAccess::Public,
                },
            )
            .await
            .expect("create ingress");

        let controller_state = state.clone();
        let controller = tokio::spawn(async move {
            let open = match server_rx.recv().await.expect("network open") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
                }
                _ => panic!("expected network open"),
            };
            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Opened,
                        stream_id: open.stream_id.clone(),
                        payload: Vec::new(),
                    },
                )
                .await
                .expect("open stream");
            let request = loop {
                let frame = match server_rx.recv().await.expect("HTTP request frame") {
                    SocketFrame::Binary(encoded) => {
                        treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                    }
                    _ => continue,
                };
                if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                    break String::from_utf8(frame.payload).expect("HTTP request text");
                }
            };
            let lower = request.to_ascii_lowercase();
            assert!(request.starts_with("GET /api/items?limit=2 HTTP/1.1\r\n"));
            assert!(lower.contains("authorization: bearer application-token\r\n"));
            assert!(lower.contains("cookie: app_session=visible\r\n"));
            assert!(!lower.contains("treer_ingress"));
            assert!(!lower.contains("x-treer-spoofed"));
            assert!(lower.contains("host: demo-"));
            assert!(lower.contains(".apps.treer.ai\r\n"));
            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\nSet-Cookie: app_session=updated; Path=/\r\n\r\nok".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::HalfClose,
                        stream_id: open.stream_id,
                        payload: Vec::new(),
                    },
                )
                .await
                .expect("close response");
        });
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        )
        .await
        .expect("identity issuer");
        let request = Request::builder()
            .uri("/api/items?limit=2")
            .header(header::HOST, &ingress.hostname)
            .header(header::AUTHORIZATION, "Bearer application-token")
            .header(
                header::COOKIE,
                "__Host-treer_ingress=private; app_session=visible",
            )
            .header("x-treer-spoofed", "false")
            .body(Body::empty())
            .expect("ingress request");
        let response = proxy_service_ingress(
            State(state),
            Extension(auth),
            Extension(test_ingress_config()),
            Extension(identity),
            request,
        )
        .await
        .unwrap_or_else(|error| panic!("ingress request: {}", error.error.message));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::SET_COOKIE),
            Some(&HeaderValue::from_static("app_session=updated; Path=/"))
        );
        controller.await.expect("join controller");
    }
}
