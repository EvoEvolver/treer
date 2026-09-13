use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Extension, Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Row, Transaction};
use subtle::ConstantTimeEq;
use treer_protocol::{
    format_machine_enrollment_key, parse_machine_enrollment_key, AgentLaunchProfile,
    AgentServerSnapshot, ApiError, AppDeployment, AppDeploymentStatus, AppDesiredState,
    CreateAgentLaunchProfileRequest, CreateAppDeploymentRequest, CreateMachineServiceRequest,
    CreateServiceIngressRequest, CreateVirtualNetworkHostRequest, MachineService,
    MachineServiceProtocol, OrganizationAuditEvent, ProtocolError, ServerInfo, ServiceIngress,
    ServiceIngressAccess, UpdateAgentLaunchProfileRequest, UpdateMachineServiceRequest,
    UpdateServiceIngressRequest, VirtualNetworkHost, WorkspaceHuman, WorkspaceInfo,
    AGENT_ID_HEADER, WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;
use uuid::Uuid;

use crate::audit::{self, NewAuditEvent, NewWorkspaceAuditEvent};

const SESSION_COOKIE: &str = "treer_session";
const ADMIN_SESSION_COOKIE: &str = "treer_admin_session";
const NATIVE_CLIENT_HEADER: &str = "x-treer-client";
const MAX_DEVICE_NAME_CHARS: usize = 128;
const SESSION_TTL_DAYS: i64 = 30;
const ADMIN_SESSION_TTL_HOURS: i64 = 8;
const PASSWORD_RESET_TTL_MINUTES: i64 = 30;
const PASSWORD_RESET_RATE_LIMIT_SECONDS: i64 = 60;
const OAUTH_STATE_TTL_MINUTES: i64 = 10;
const MACHINE_ENROLLMENT_TTL_MINUTES: i64 = 10;
const AGENT_CREDENTIAL_CACHE_TTL: StdDuration = StdDuration::from_secs(5);
const INGRESS_AUTH_CODE_TTL_MINUTES: i64 = 5;
const INGRESS_SESSION_TTL_HOURS: i64 = 12;
const APP_OAUTH_CODE_TTL_MINUTES: i64 = 5;
const MAX_LAUNCH_PROFILE_DESCRIPTION_CHARS: usize = 1_000;
const MAX_LAUNCH_PROFILE_COMMAND_BYTES: usize = 4_096;
const MAX_LAUNCH_PROFILE_CWD_BYTES: usize = 4_096;
const MAX_LAUNCH_PROFILE_ARGS: usize = 128;
const MAX_LAUNCH_PROFILE_ARG_BYTES: usize = 4_096;
const MAX_LAUNCH_PROFILE_ARGS_BYTES: usize = 64 * 1024;
const DEFAULT_AGENT_LAUNCH_PROFILES: [(&str, &str, &str); 4] = [
    ("Codex", "OpenAI Codex", "codex"),
    ("Claude", "Anthropic Claude Code", "claude"),
    ("Pi", "Pi coding agent", "pi"),
    ("OpenCode", "OpenCode", "opencode"),
];

pub(crate) struct ProfileMutationActor<'a> {
    pub kind: &'a str,
    pub id: Option<&'a str>,
    pub label: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeletedWorkspace {
    pub workspace_id: String,
    pub organization_id: String,
    pub name: String,
    pub machine_count: i64,
    pub agent_count: i64,
    pub app_count: i64,
}

#[derive(Clone)]
pub struct AuthStore {
    pool: PgPool,
    admin_password: Arc<str>,
    app_public_url: Url,
    proxy_public_url: Url,
    secure_cookies: bool,
    disabled: bool,
    email_sender: Option<CloudflareEmailSender>,
    oauth: Arc<OAuthConfig>,
    oauth_client: reqwest::Client,
    virtual_hosts: Arc<tokio::sync::RwLock<HashMap<String, HashMap<String, VirtualNetworkHost>>>>,
    virtual_hosts_update: Arc<tokio::sync::Mutex<()>>,
    virtual_hosts_revision: Arc<AtomicU64>,
    agent_credentials: Arc<tokio::sync::RwLock<HashMap<String, AgentCredentialRecord>>>,
    service_ingresses: Arc<tokio::sync::RwLock<HashMap<String, ResolvedServiceIngress>>>,
    service_ingresses_update: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedServiceIngress {
    pub ingress: ServiceIngress,
    pub service: MachineService,
}

#[derive(Clone, Debug)]
pub(crate) struct ConsumedIngressAuthorization {
    pub session_token: String,
    pub return_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppOAuthGrant {
    pub workspace_id: String,
    pub service_id: String,
    pub user_id: String,
    pub preferred_name: String,
    pub role: String,
}

pub struct CloudflareEmailConfig {
    pub account_id: String,
    pub api_token: String,
    pub from: String,
}

#[derive(Clone)]
pub struct OAuthProviderConfig {
    client_id: Arc<str>,
    client_secret: Arc<str>,
    authorize_url: Url,
    token_url: Url,
    user_url: Url,
    emails_url: Option<Url>,
}

#[derive(Clone)]
pub struct OAuthConfig {
    github: Option<OAuthProviderConfig>,
    google: Option<OAuthProviderConfig>,
    invitation_required: bool,
}

pub struct AuthStoreConfig {
    pub app_public_url: Url,
    pub proxy_public_url: Url,
    pub secure_cookies: bool,
    pub disabled: bool,
    pub email: Option<CloudflareEmailConfig>,
    pub oauth: OAuthConfig,
}

impl OAuthProviderConfig {
    pub fn github(client_id: String, client_secret: String) -> anyhow::Result<Self> {
        Self::new(
            client_id,
            client_secret,
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            "https://api.github.com/user",
            Some("https://api.github.com/user/emails"),
        )
    }

    pub fn google(client_id: String, client_secret: String) -> anyhow::Result<Self> {
        Self::new(
            client_id,
            client_secret,
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://openidconnect.googleapis.com/v1/userinfo",
            None,
        )
    }

    fn new(
        client_id: String,
        client_secret: String,
        authorize_url: &str,
        token_url: &str,
        user_url: &str,
        emails_url: Option<&str>,
    ) -> anyhow::Result<Self> {
        if client_id.trim().is_empty() || client_secret.is_empty() {
            anyhow::bail!("OAuth client ID and secret must not be empty");
        }
        Ok(Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            authorize_url: Url::parse(authorize_url)?,
            token_url: Url::parse(token_url)?,
            user_url: Url::parse(user_url)?,
            emails_url: emails_url.map(Url::parse).transpose()?,
        })
    }
}

impl OAuthConfig {
    pub fn new(
        github: Option<OAuthProviderConfig>,
        google: Option<OAuthProviderConfig>,
        invitation_required: bool,
    ) -> Self {
        Self {
            github,
            google,
            invitation_required,
        }
    }

    fn provider(&self, provider: &str) -> Option<&OAuthProviderConfig> {
        match provider {
            "github" => self.github.as_ref(),
            "google" => self.google.as_ref(),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct CloudflareEmailSender {
    client: reqwest::Client,
    endpoint: Url,
    api_token: Arc<str>,
    from: Arc<str>,
}

pub(crate) struct PendingPasswordReset {
    pub token_id: String,
    pub recipient: String,
    pub url: Url,
}

#[derive(Clone, Debug)]
pub struct CurrentSession {
    pub token: String,
    pub user_id: String,
    pub email: String,
    pub preferred_name: String,
}

#[derive(Clone, Debug)]
pub struct AdminSession {
    pub token: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OrganizationInfo {
    pub organization_id: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OrganizationMember {
    pub user_id: String,
    pub email: String,
    pub preferred_name: String,
    pub role: String,
    pub joined_at: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OrganizationGroup {
    pub group_id: String,
    pub organization_id: String,
    pub name: String,
    pub member_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkspaceAccessMember {
    pub user_id: String,
    pub preferred_name: String,
    pub email: String,
    pub role: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkspaceAccessGroup {
    pub group_id: String,
    pub name: String,
    pub role: String,
    pub member_count: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkspaceAccessInfo {
    pub workspace_id: String,
    pub access_mode: String,
    pub current_role: String,
    pub members: Vec<WorkspaceAccessMember>,
    pub groups: Vec<WorkspaceAccessGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineSession {
    pub server_id: Option<String>,
    pub workspace_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSession {
    pub agent_id: String,
    pub server_id: String,
    pub workspace_id: String,
}

#[derive(Clone)]
struct AgentCredentialRecord {
    workspace_id: String,
    server_id: String,
    secret_hash: String,
    cached_at: Instant,
}

impl MachineSession {
    pub fn allows_workspace(&self, workspace_id: &str) -> bool {
        self.workspace_id
            .as_ref()
            .is_none_or(|expected| expected == workspace_id)
    }

    pub fn allows_server(&self, workspace_id: &str, server_id: &str) -> bool {
        self.allows_workspace(workspace_id)
            && self
                .server_id
                .as_ref()
                .is_none_or(|expected| expected == server_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineEnrollmentClaim {
    pub workspace_id: String,
    pub server_id: String,
    pub machine_token: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    email: String,
    password: String,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    invite: Option<String>,
    email: String,
    preferred_name: String,
    password: String,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeClientAttribution {
    client: String,
    device_id: Option<String>,
    device_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RequestPasswordResetRequest {
    email: String,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    token: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct OAuthStartQuery {
    invite: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
struct OAuthProfile {
    provider: &'static str,
    subject: String,
    email: String,
    preferred_name: String,
}

struct RegistrationInvitation {
    token: String,
    kind: String,
    organization_id: Option<String>,
    role: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    id: u64,
    login: String,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

#[derive(Debug, Deserialize)]
struct GoogleUser {
    sub: String,
    email: String,
    email_verified: bool,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    email: String,
    preferred_name: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminLoginRequest {
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrganizationRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
pub struct RenameOrganizationRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemberRoleRequest {
    role: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceAccessRequest {
    access_mode: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspaceGrantRequest {
    role: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrganizationGroupRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
pub struct AuditEventsQuery {
    workspace_id: Option<String>,
    before: Option<i64>,
    #[serde(default = "default_audit_limit")]
    limit: u16,
}

const fn default_audit_limit() -> u16 {
    50
}

impl CloudflareEmailSender {
    fn new(config: CloudflareEmailConfig) -> anyhow::Result<Self> {
        if config.account_id.trim().is_empty() {
            anyhow::bail!("Cloudflare account ID must not be empty");
        }
        if config.api_token.is_empty() {
            anyhow::bail!("Cloudflare API token must not be empty");
        }
        if !config.from.contains('@')
            || config
                .from
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            anyhow::bail!("password reset sender must be an email address");
        }
        let mut endpoint = Url::parse("https://api.cloudflare.com/client/v4/accounts/")?;
        endpoint
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Cloudflare API base URL cannot be a base"))?
            .push(&config.account_id)
            .push("email")
            .push("sending")
            .push("send");
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(StdDuration::from_secs(10))
                .build()?,
            endpoint,
            api_token: config.api_token.into(),
            from: config.from.into(),
        })
    }

    async fn send(
        &self,
        recipient: &str,
        subject: &str,
        html: String,
        text: String,
    ) -> anyhow::Result<()> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(self.api_token.as_ref())
            .json(&json!({
                "to": recipient,
                "from": self.from.as_ref(),
                "subject": subject,
                "html": html,
                "text": text,
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<CloudflareEmailResponse>().await?;
        if !status.is_success() || !body.success {
            let message = body
                .errors
                .first()
                .map(|error| error.message.as_str())
                .unwrap_or("unknown Cloudflare email error");
            anyhow::bail!("Cloudflare email API returned {status}: {message}");
        }
        Ok(())
    }

    async fn send_password_reset(&self, recipient: &str, reset_url: &Url) -> anyhow::Result<()> {
        let text = format!(
            "Reset your Treer password\n\nOpen this link within 30 minutes:\n\n{}\n\nIf you did not request this, you can ignore this email.",
            reset_url.as_str()
        );
        let html_url = escape_html(reset_url.as_str());
        let html = format!(
            "<h1>Reset your Treer password</h1><p>Open the link below within 30 minutes.</p><p><a href=\"{html_url}\">Reset password</a></p><p>If you did not request this, you can ignore this email.</p>"
        );
        self.send(recipient, "Reset your Treer password", html, text)
            .await
    }

    async fn send_welcome(
        &self,
        recipient: &str,
        preferred_name: &str,
        app_url: &Url,
    ) -> anyhow::Result<()> {
        let text = format!(
            "Hi {preferred_name},\n\nYour Treer account is ready.\n\nOpen Treer: {}",
            app_url.as_str()
        );
        let preferred_name = escape_html(preferred_name);
        let app_url = escape_html(app_url.as_str());
        let html = format!(
            "<h1>Welcome to Treer</h1><p>Hi {preferred_name}, your account is ready.</p><p><a href=\"{app_url}\">Open Treer</a></p>"
        );
        self.send(recipient, "Welcome to Treer", html, text).await
    }
}

#[derive(Debug, Deserialize)]
struct CloudflareEmailResponse {
    success: bool,
    #[serde(default)]
    errors: Vec<CloudflareEmailError>,
}

#[derive(Debug, Deserialize)]
struct CloudflareEmailError {
    message: String,
}

impl AuthStore {
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    pub(crate) fn app_public_url(&self) -> &Url {
        &self.app_public_url
    }

    pub(crate) fn has_email_sender(&self) -> bool {
        self.email_sender.is_some()
    }

    pub(crate) fn spawn_password_reset_email(&self, recipient: String, url: Url) {
        let Some(sender) = self.email_sender.clone() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(error) = sender.send_password_reset(&recipient, &url).await {
                tracing::error!(%error, "failed to send admin-issued password reset email");
            }
        });
    }

    pub(crate) async fn record_workspace_audit(
        &self,
        event: NewWorkspaceAuditEvent<'_>,
    ) -> Result<(), AuthFailure> {
        audit::record_workspace(&self.pool, event)
            .await
            .map_err(AuthFailure::database)
    }

    pub async fn open(
        database_url: &str,
        admin_password: String,
        config: AuthStoreConfig,
    ) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        let store = Self {
            pool,
            admin_password: admin_password.into(),
            app_public_url: config.app_public_url,
            proxy_public_url: config.proxy_public_url,
            secure_cookies: config.secure_cookies,
            disabled: config.disabled,
            email_sender: config.email.map(CloudflareEmailSender::new).transpose()?,
            oauth: Arc::new(config.oauth),
            oauth_client: reqwest::Client::builder()
                .timeout(StdDuration::from_secs(10))
                .user_agent("Treer/0.1")
                .build()?,
            virtual_hosts: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            virtual_hosts_update: Arc::new(tokio::sync::Mutex::new(())),
            virtual_hosts_revision: Arc::new(AtomicU64::new(0)),
            agent_credentials: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            service_ingresses: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            service_ingresses_update: Arc::new(tokio::sync::Mutex::new(())),
        };
        store.initialize_schema().await?;
        store.refresh_virtual_network_hosts().await?;
        store.refresh_service_ingresses().await?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) async fn for_test(admin_password: &str) -> Self {
        let database_url = std::env::var("TREER_TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://treer:treer@127.0.0.1:55432/treer_test".to_string());
        let schema = format!("test_{}", Uuid::new_v4().simple());
        let setup_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect to test PostgreSQL; start the documented Docker test database");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&setup_pool)
            .await
            .expect("create isolated test schema");
        setup_pool.close().await;

        let search_path = format!("SET search_path TO {schema}");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .expect("connect to isolated test schema");
        let store = Self {
            pool,
            admin_password: admin_password.to_string().into(),
            app_public_url: Url::parse("https://app.treer.example/").expect("valid URL"),
            proxy_public_url: Url::parse("https://proxy.treer.example/").expect("valid URL"),
            secure_cookies: true,
            disabled: false,
            email_sender: None,
            oauth: Arc::new(OAuthConfig::new(None, None, true)),
            oauth_client: reqwest::Client::new(),
            virtual_hosts: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            virtual_hosts_update: Arc::new(tokio::sync::Mutex::new(())),
            virtual_hosts_revision: Arc::new(AtomicU64::new(0)),
            agent_credentials: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            service_ingresses: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            service_ingresses_update: Arc::new(tokio::sync::Mutex::new(())),
        };
        store
            .initialize_schema()
            .await
            .expect("initialize database schema");
        store
            .refresh_virtual_network_hosts()
            .await
            .expect("load virtual hosts");
        store
            .refresh_service_ingresses()
            .await
            .expect("load service ingresses");
        store
    }

    #[cfg(test)]
    pub(crate) async fn seed_test_workspace(&self, workspace_id: &str) {
        let organization_id = format!("org_{workspace_id}");
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO organizations(organization_id, name, created_at, created_by) \
             VALUES($1, $2, $3, 'test')",
        )
        .bind(&organization_id)
        .bind(format!("{workspace_id} organization"))
        .bind(&now)
        .execute(&self.pool)
        .await
        .expect("seed organization");
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             VALUES($1, $2, $3, $4, 'test')",
        )
        .bind(workspace_id)
        .bind(organization_id)
        .bind(workspace_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .expect("seed workspace");
    }

    async fn initialize_schema(&self) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('treer_schema'))")
            .execute(&mut *transaction)
            .await?;
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub(crate) async fn load_or_create_proxy_secret(
        &self,
        name: &str,
        candidate: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        sqlx::query(
            "INSERT INTO proxy_secrets(name, value, created_at) VALUES($1, $2, $3) \
             ON CONFLICT DO NOTHING",
        )
        .bind(name)
        .bind(candidate)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        sqlx::query_scalar("SELECT value FROM proxy_secrets WHERE name = $1")
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }
}
mod accounts;
mod apps;
mod handlers;
mod machines;
mod network;
mod organizations;

pub(crate) use handlers::*;
#[derive(Debug)]
pub struct AuthFailure {
    status: StatusCode,
    error: ProtocolError,
}

impl AuthFailure {
    pub(crate) fn not_found(code: &str, message: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    fn unauthorized(code: &str, message: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    fn forbidden(code: &str, message: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    fn bad_request(code: &str, message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn conflict(code: &str, message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub(crate) fn too_many_requests(code: &str, message: &str) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, code, message)
    }

    fn internal(code: &str, message: String) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
    }

    pub(crate) fn database(error: sqlx::Error) -> Self {
        tracing::error!(%error, "authentication database error");
        Self::internal("database_error", "database operation failed".to_string())
    }

    fn header(error: axum::http::header::InvalidHeaderValue) -> Self {
        tracing::error!(%error, "failed to encode session cookie");
        Self::internal("session_error", "failed to create session".to_string())
    }

    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            error: ProtocolError::new(code, message),
        }
    }

    pub fn into_parts(self) -> (StatusCode, ProtocolError) {
        (self.status, self.error)
    }
}

impl IntoResponse for AuthFailure {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.error })).into_response()
    }
}

#[cfg(test)]
#[path = "auth/tests.rs"]
mod tests;
