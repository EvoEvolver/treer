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
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    invite: Option<String>,
    email: String,
    preferred_name: String,
    password: String,
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

    pub async fn all_workspaces(&self) -> Result<Vec<WorkspaceInfo>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT workspace_id, name, created_at FROM workspaces \
             WHERE deleted_at IS NULL ORDER BY workspace_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(workspace_from_row).collect()
    }

    pub async fn list_organizations(
        &self,
        user_id: &str,
    ) -> Result<Vec<OrganizationInfo>, AuthFailure> {
        let rows = if self.disabled {
            sqlx::query(
                "SELECT organization_id, name, created_at, 'owner' AS role \
                 FROM organizations ORDER BY lower(name), organization_id",
            )
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT o.organization_id, o.name, o.created_at, m.role \
                 FROM organizations o \
                 JOIN organization_members m ON m.organization_id = o.organization_id \
                 WHERE m.user_id = $1 \
                 ORDER BY lower(o.name), o.organization_id",
            )
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| OrganizationInfo {
                organization_id: row.get("organization_id"),
                name: row.get("name"),
                role: row.get("role"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    pub async fn create_organization(
        &self,
        user_id: &str,
        name: &str,
    ) -> Result<OrganizationInfo, AuthFailure> {
        let name = validate_resource_name(name, "organization")?;
        let organization_id = format!("org_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO organizations(organization_id, name, created_at, created_by) \
             VALUES($1, $2, $3, $4)",
        )
        .bind(&organization_id)
        .bind(&name)
        .bind(&now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             SELECT $1, id, 'owner', $2 FROM users WHERE id = $3",
        )
        .bind(&organization_id)
        .bind(&now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id: &organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "organization.created",
                resource_kind: "organization",
                resource_id: &organization_id,
                resource_name: Some(&name),
                payload: json!({}),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(OrganizationInfo {
            organization_id,
            name,
            role: "owner".to_string(),
            created_at: now,
        })
    }

    pub async fn rename_organization(
        &self,
        organization_id: &str,
        user_id: &str,
        name: &str,
    ) -> Result<OrganizationInfo, AuthFailure> {
        let role = self.require_manager(organization_id, user_id).await?;
        let name = validate_resource_name(name, "organization")?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let old_name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM organizations WHERE organization_id = $1 FOR UPDATE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| {
            AuthFailure::not_found("organization_not_found", "organization does not exist")
        })?;
        let result = sqlx::query("UPDATE organizations SET name = $1 WHERE organization_id = $2")
            .bind(&name)
            .bind(organization_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "organization_not_found",
                "organization does not exist",
            ));
        }
        let row = sqlx::query("SELECT created_at FROM organizations WHERE organization_id = $1")
            .bind(organization_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "organization.renamed",
                resource_kind: "organization",
                resource_id: organization_id,
                resource_name: Some(&name),
                payload: json!({ "old_name": old_name, "new_name": name }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(OrganizationInfo {
            organization_id: organization_id.to_string(),
            name,
            role,
            created_at: row.get("created_at"),
        })
    }

    pub async fn active_machine_count(&self) -> Result<i64, AuthFailure> {
        sqlx::query_scalar("SELECT COUNT(*) FROM machines WHERE revoked_at IS NULL")
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)
    }

    pub async fn user_count(&self) -> Result<i64, AuthFailure> {
        sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)
    }

    pub async fn organization_count(&self) -> Result<i64, AuthFailure> {
        sqlx::query_scalar("SELECT COUNT(*) FROM organizations")
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)
    }

    pub async fn require_organization_member(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<String, AuthFailure> {
        self.membership_role(organization_id, user_id)
            .await?
            .ok_or_else(|| {
                AuthFailure::forbidden(
                    "organization_access_denied",
                    "you are not a member of this organization",
                )
            })
    }

    pub async fn require_workspace_member(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<(), AuthFailure> {
        self.workspace_member_role(workspace_id, user_id).await?;
        Ok(())
    }

    pub async fn workspace_member_role(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<String, AuthFailure> {
        let organization_id = sqlx::query_scalar::<_, String>(
            "SELECT organization_id FROM workspaces \
             WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        self.require_organization_member(&organization_id, user_id)
            .await
    }

    pub async fn list_members(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Vec<OrganizationMember>, AuthFailure> {
        self.require_organization_member(organization_id, user_id)
            .await?;
        let rows = sqlx::query(
            "SELECT u.id AS user_id, u.email, u.preferred_name, m.role, m.joined_at \
             FROM organization_members m JOIN users u ON u.id = m.user_id \
             WHERE m.organization_id = $1 \
             ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END, \
             lower(u.preferred_name), lower(u.email)",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| OrganizationMember {
                user_id: row.get("user_id"),
                email: row.get("email"),
                preferred_name: row.get("preferred_name"),
                role: row.get("role"),
                joined_at: row.get("joined_at"),
            })
            .collect())
    }

    pub async fn list_workspace_humans(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<WorkspaceHuman>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT u.id AS user_id, u.preferred_name, m.role \
             FROM workspaces w \
             JOIN organization_members m ON m.organization_id = w.organization_id \
             JOIN users u ON u.id = m.user_id \
             WHERE w.workspace_id = $1 AND w.deleted_at IS NULL \
             ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END, \
                      lower(u.preferred_name), u.id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| WorkspaceHuman {
                user_id: row.get("user_id"),
                preferred_name: row.get("preferred_name"),
                role: row.get("role"),
            })
            .collect())
    }

    pub async fn list_workspaces(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Vec<WorkspaceInfo>, AuthFailure> {
        self.require_organization_member(organization_id, user_id)
            .await?;
        let rows = sqlx::query(
            "SELECT workspace_id, name, created_at FROM workspaces \
             WHERE organization_id = $1 AND deleted_at IS NULL \
             ORDER BY lower(name), workspace_id",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(workspace_from_row).collect()
    }

    pub async fn create_workspace(
        &self,
        organization_id: &str,
        workspace_id: &str,
        name: &str,
        user_id: &str,
    ) -> Result<WorkspaceInfo, AuthFailure> {
        self.require_organization_member(organization_id, user_id)
            .await?;
        let name = validate_resource_name(name, "workspace")?;
        let now = Utc::now();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(workspace_id)
        .bind(organization_id)
        .bind(&name)
        .bind(now.to_rfc3339())
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("workspace_exists", "workspace already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        insert_default_agent_launch_profiles(&mut transaction, workspace_id, user_id, &now).await?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: Some(workspace_id),
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "workspace.created",
                resource_kind: "workspace",
                resource_id: workspace_id,
                resource_name: Some(&name),
                payload: json!({}),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            name,
            created_at: now,
        })
    }

    pub async fn rename_workspace(
        &self,
        workspace_id: &str,
        user_id: &str,
        name: &str,
    ) -> Result<WorkspaceInfo, AuthFailure> {
        self.require_workspace_member(workspace_id, user_id).await?;
        let name = validate_resource_name(name, "workspace")?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT organization_id, name FROM workspaces \
             WHERE workspace_id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(workspace_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        let organization_id: String = row.get("organization_id");
        let old_name: String = row.get("name");
        sqlx::query(
            "UPDATE workspaces SET name = $1 \
             WHERE workspace_id = $2 AND deleted_at IS NULL",
        )
        .bind(&name)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id: &organization_id,
                workspace_id: Some(workspace_id),
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "workspace.renamed",
                resource_kind: "workspace",
                resource_id: workspace_id,
                resource_name: Some(&name),
                payload: json!({ "old_name": old_name, "new_name": name }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        let info = workspace_from_row(
            sqlx::query(
                "SELECT workspace_id, name, created_at FROM workspaces \
                 WHERE workspace_id = $1 AND deleted_at IS NULL",
            )
            .bind(workspace_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?,
        )?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(info)
    }

    pub async fn delete_workspace(
        &self,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<DeletedWorkspace, AuthFailure> {
        let organization_id = sqlx::query_scalar::<_, String>(
            "SELECT organization_id FROM workspaces \
             WHERE workspace_id = $1 AND deleted_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        self.require_manager(&organization_id, user_id).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT organization_id, name FROM workspaces \
             WHERE workspace_id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(workspace_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        let organization_id: String = row.get("organization_id");
        let name: String = row.get("name");
        let machine_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM machines WHERE workspace_id = $1 AND revoked_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if machine_count != 0 {
            return Err(AuthFailure::conflict(
                "workspace_has_machines",
                "delete all machines in this workspace first",
            ));
        }
        let agent_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_credentials WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let app_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM app_deployments WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE agent_credentials SET revoked_at = $1 \
             WHERE workspace_id = $2 AND revoked_at IS NULL",
        )
        .bind(&now)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id: &organization_id,
                workspace_id: Some(workspace_id),
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "workspace.deleted",
                resource_kind: "workspace",
                resource_id: workspace_id,
                resource_name: Some(&name),
                payload: json!({
                    "machine_count": machine_count,
                    "agent_count": agent_count,
                    "app_count": app_count,
                }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE workspaces SET deleted_at = $1, deleted_by = $2 \
             WHERE workspace_id = $3 AND deleted_at IS NULL",
        )
        .bind(&now)
        .bind(user_id)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.agent_credentials
            .write()
            .await
            .retain(|_, record| record.workspace_id != workspace_id);
        let _update = self.service_ingresses_update.lock().await;
        self.service_ingresses
            .write()
            .await
            .retain(|_, resolved| resolved.ingress.workspace_id != workspace_id);
        if self
            .virtual_hosts
            .write()
            .await
            .remove(workspace_id)
            .is_some()
        {
            self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        }
        Ok(DeletedWorkspace {
            workspace_id: workspace_id.to_string(),
            organization_id,
            name,
            machine_count,
            agent_count,
            app_count,
        })
    }

    pub async fn list_agent_launch_profiles(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<AgentLaunchProfile>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT profile_id, workspace_id, name, description, cwd, command, args, \
             created_at, created_by, updated_at, updated_by FROM agent_launch_profiles \
             WHERE workspace_id = $1 ORDER BY lower(name), profile_id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter()
            .map(agent_launch_profile_from_row)
            .collect()
    }

    pub async fn resolve_agent_launch_profile(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<AgentLaunchProfile, AuthFailure> {
        let row = sqlx::query(
            "SELECT profile_id, workspace_id, name, description, cwd, command, args, \
             created_at, created_by, updated_at, updated_by FROM agent_launch_profiles \
             WHERE workspace_id = $1 AND profile_id = $2",
        )
        .bind(workspace_id)
        .bind(target)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let row = match row {
            Some(row) => row,
            None => sqlx::query(
                "SELECT profile_id, workspace_id, name, description, cwd, command, args, \
                 created_at, created_by, updated_at, updated_by FROM agent_launch_profiles \
                 WHERE workspace_id = $1 AND lower(name) = lower($2)",
            )
            .bind(workspace_id)
            .bind(target.trim())
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?
            .ok_or_else(|| {
                AuthFailure::not_found(
                    "launch_profile_not_found",
                    "agent launch profile does not exist",
                )
            })?,
        };
        agent_launch_profile_from_row(row)
    }

    pub async fn create_agent_launch_profile(
        &self,
        workspace_id: &str,
        actor: ProfileMutationActor<'_>,
        request: CreateAgentLaunchProfileRequest,
    ) -> Result<AgentLaunchProfile, AuthFailure> {
        let now = Utc::now();
        let profile = AgentLaunchProfile {
            profile_id: format!("alp_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            name: validate_resource_name(&request.name, "launch profile")?,
            description: validate_launch_profile_description(&request.description)?,
            cwd: validate_launch_profile_cwd(&request.cwd)?,
            command: validate_launch_profile_command(&request.command)?,
            args: validate_launch_profile_args(request.args)?,
            created_at: now,
            created_by: actor.label.to_string(),
            updated_at: now,
            updated_by: actor.label.to_string(),
        };
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO agent_launch_profiles(\
             profile_id, workspace_id, name, description, cwd, command, args, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&profile.profile_id)
        .bind(&profile.workspace_id)
        .bind(&profile.name)
        .bind(&profile.description)
        .bind(&profile.cwd)
        .bind(&profile.command)
        .bind(json!(&profile.args))
        .bind(profile.created_at.to_rfc3339())
        .bind(&profile.created_by)
        .bind(profile.updated_at.to_rfc3339())
        .bind(&profile.updated_by)
        .execute(&mut *transaction)
        .await
        .map_err(launch_profile_write_error)?;
        insert_launch_profile_audit(&mut transaction, &profile, actor, "launch_profile.created")
            .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(profile)
    }

    pub async fn update_agent_launch_profile(
        &self,
        workspace_id: &str,
        target: &str,
        actor: ProfileMutationActor<'_>,
        request: UpdateAgentLaunchProfileRequest,
    ) -> Result<AgentLaunchProfile, AuthFailure> {
        let current = self
            .resolve_agent_launch_profile(workspace_id, target)
            .await?;
        let profile = AgentLaunchProfile {
            name: request
                .name
                .as_deref()
                .map(|name| validate_resource_name(name, "launch profile"))
                .transpose()?
                .unwrap_or(current.name),
            description: request
                .description
                .as_deref()
                .map(validate_launch_profile_description)
                .transpose()?
                .unwrap_or(current.description),
            cwd: request
                .cwd
                .as_deref()
                .map(validate_launch_profile_cwd)
                .transpose()?
                .unwrap_or(current.cwd),
            command: request
                .command
                .as_deref()
                .map(validate_launch_profile_command)
                .transpose()?
                .unwrap_or(current.command),
            args: request
                .args
                .map(validate_launch_profile_args)
                .transpose()?
                .unwrap_or(current.args),
            updated_at: Utc::now(),
            updated_by: actor.label.to_string(),
            ..current
        };
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let result = sqlx::query(
            "UPDATE agent_launch_profiles SET name = $1, description = $2, cwd = $3, \
             command = $4, args = $5, updated_at = $6, updated_by = $7 \
             WHERE workspace_id = $8 AND profile_id = $9",
        )
        .bind(&profile.name)
        .bind(&profile.description)
        .bind(&profile.cwd)
        .bind(&profile.command)
        .bind(json!(&profile.args))
        .bind(profile.updated_at.to_rfc3339())
        .bind(&profile.updated_by)
        .bind(workspace_id)
        .bind(&profile.profile_id)
        .execute(&mut *transaction)
        .await
        .map_err(launch_profile_write_error)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "launch_profile_not_found",
                "agent launch profile does not exist",
            ));
        }
        insert_launch_profile_audit(&mut transaction, &profile, actor, "launch_profile.updated")
            .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(profile)
    }

    pub async fn delete_agent_launch_profile(
        &self,
        workspace_id: &str,
        target: &str,
        actor: ProfileMutationActor<'_>,
    ) -> Result<AgentLaunchProfile, AuthFailure> {
        let profile = self
            .resolve_agent_launch_profile(workspace_id, target)
            .await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let result = sqlx::query(
            "DELETE FROM agent_launch_profiles WHERE workspace_id = $1 AND profile_id = $2",
        )
        .bind(workspace_id)
        .bind(&profile.profile_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "launch_profile_not_found",
                "agent launch profile does not exist",
            ));
        }
        insert_launch_profile_audit(&mut transaction, &profile, actor, "launch_profile.deleted")
            .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(profile)
    }

    pub async fn list_app_deployments(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<AppDeployment>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by FROM app_deployments \
             WHERE workspace_id = $1 ORDER BY lower(name), app_id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(app_deployment_from_row).collect()
    }

    pub async fn list_app_deployments_for_server(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<Vec<AppDeployment>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by FROM app_deployments \
             WHERE workspace_id = $1 AND server_id = $2 ORDER BY app_id",
        )
        .bind(workspace_id)
        .bind(server_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(app_deployment_from_row).collect()
    }

    pub async fn resolve_app_deployment(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<AppDeployment, AuthFailure> {
        let row = sqlx::query(
            "SELECT app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by FROM app_deployments \
             WHERE workspace_id = $1 AND (app_id = $2 OR lower(name) = lower($2))",
        )
        .bind(workspace_id)
        .bind(target.trim())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("app_not_found", "App does not exist"))?;
        app_deployment_from_row(row)
    }

    pub async fn resolve_app_deployment_by_runtime(
        &self,
        workspace_id: &str,
        runtime_agent_id: &str,
    ) -> Result<Option<AppDeployment>, AuthFailure> {
        let row = sqlx::query(
            "SELECT app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by FROM app_deployments \
             WHERE workspace_id = $1 AND runtime_agent_id = $2",
        )
        .bind(workspace_id)
        .bind(runtime_agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        row.map(app_deployment_from_row).transpose()
    }

    pub async fn create_app_deployment(
        &self,
        workspace_id: &str,
        actor: &str,
        server_id: String,
        request: CreateAppDeploymentRequest,
    ) -> Result<AppDeployment, AuthFailure> {
        let update_guard = self.virtual_hosts_update.lock().await;
        let name = validate_resource_name(&request.name, "App")?;
        let command = validate_launch_profile_command(&request.command)?;
        let args = validate_launch_profile_args(request.args)?;
        let cwd = validate_launch_profile_cwd(&request.cwd)?;
        if request.port == 0 {
            return Err(AuthFailure::bad_request(
                "invalid_app",
                "App port must be between 1 and 65535",
            ));
        }
        let hostname = normalize_virtual_hostname(&request.hostname)?;
        let now = Utc::now();
        let service = MachineService {
            service_id: format!("svc_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            name: name.clone(),
            server_id: server_id.clone(),
            target_agent_id: None,
            target_host: "127.0.0.1".to_string(),
            target_port: request.port,
            protocol: MachineServiceProtocol::Http,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        let deployment = AppDeployment {
            app_id: format!("app_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            name,
            server_id,
            command,
            args,
            cwd,
            port: request.port,
            hostname: hostname.clone(),
            service_id: service.service_id.clone(),
            public_url: None,
            desired_state: AppDesiredState::Running,
            runtime_agent_id: None,
            restart_count: 0,
            status: AppDeploymentStatus::Pending,
            pid: None,
            exit_code: None,
            last_error: None,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO machine_services(service_id, workspace_id, name, server_id, target_agent_id, \
             target_host, target_port, protocol, created_at, created_by, updated_at, updated_by) \
             VALUES($1, $2, $3, $4, NULL, $5, $6, 'http', $7, $8, $9, $10)",
        )
        .bind(&service.service_id)
        .bind(&service.workspace_id)
        .bind(&service.name)
        .bind(&service.server_id)
        .bind(&service.target_host)
        .bind(i64::from(service.target_port))
        .bind(service.created_at.to_rfc3339())
        .bind(&service.created_by)
        .bind(service.updated_at.to_rfc3339())
        .bind(&service.updated_by)
        .execute(&mut *transaction)
        .await
        .map_err(app_deployment_write_error)?;
        sqlx::query(
            "INSERT INTO virtual_network_hosts(workspace_id, hostname, service_id, created_at, created_by) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(workspace_id)
        .bind(&hostname)
        .bind(&service.service_id)
        .bind(now.to_rfc3339())
        .bind(actor)
        .execute(&mut *transaction)
        .await
        .map_err(app_deployment_write_error)?;
        sqlx::query(
            "INSERT INTO app_deployments(app_id, workspace_id, name, server_id, command, args, cwd, \
             port, hostname, service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by) \
             VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'running', NULL, 0, NULL, $11, $12, $13, $14)",
        )
        .bind(&deployment.app_id)
        .bind(&deployment.workspace_id)
        .bind(&deployment.name)
        .bind(&deployment.server_id)
        .bind(&deployment.command)
        .bind(serde_json::to_value(&deployment.args).map_err(|error| {
            AuthFailure::internal("app_encode_error", error.to_string())
        })?)
        .bind(&deployment.cwd)
        .bind(i64::from(deployment.port))
        .bind(&deployment.hostname)
        .bind(&deployment.service_id)
        .bind(deployment.created_at.to_rfc3339())
        .bind(&deployment.created_by)
        .bind(deployment.updated_at.to_rfc3339())
        .bind(&deployment.updated_by)
        .execute(&mut *transaction)
        .await
        .map_err(app_deployment_write_error)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        drop(update_guard);
        self.refresh_virtual_network_hosts()
            .await
            .map_err(|error| {
                AuthFailure::internal("virtual_host_refresh_failed", format!("{error:#}"))
            })?;
        Ok(deployment)
    }

    pub async fn claim_app_runtime(
        &self,
        workspace_id: &str,
        app_id: &str,
        expected_runtime_agent_id: Option<&str>,
        runtime_agent_id: &str,
        actor: &str,
    ) -> Result<Option<AppDeployment>, AuthFailure> {
        let row = sqlx::query(
            "UPDATE app_deployments SET runtime_agent_id = $1, \
             restart_count = restart_count + CASE WHEN runtime_agent_id IS NULL THEN 0 ELSE 1 END, \
             last_error = NULL, updated_at = $2, updated_by = $3 \
             WHERE workspace_id = $4 AND app_id = $5 AND desired_state = 'running' \
             AND runtime_agent_id IS NOT DISTINCT FROM $6 \
             RETURNING app_id, workspace_id, name, server_id, command, args, cwd, port, hostname, \
             service_id, desired_state, runtime_agent_id, restart_count, last_error, \
             created_at, created_by, updated_at, updated_by",
        )
        .bind(runtime_agent_id)
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .bind(workspace_id)
        .bind(app_id)
        .bind(expected_runtime_agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        row.map(app_deployment_from_row).transpose()
    }

    pub async fn set_app_desired_state(
        &self,
        workspace_id: &str,
        target: &str,
        desired_state: AppDesiredState,
        actor: &str,
    ) -> Result<AppDeployment, AuthFailure> {
        let current = self.resolve_app_deployment(workspace_id, target).await?;
        let row = sqlx::query(
            "UPDATE app_deployments SET desired_state = $1, updated_at = $2, updated_by = $3 \
             WHERE workspace_id = $4 AND app_id = $5 RETURNING app_id, workspace_id, name, server_id, \
             command, args, cwd, port, hostname, service_id, desired_state, runtime_agent_id, \
             restart_count, last_error, created_at, created_by, updated_at, updated_by",
        )
        .bind(app_desired_state_str(desired_state))
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .bind(workspace_id)
        .bind(&current.app_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        app_deployment_from_row(row)
    }

    pub async fn set_app_last_error(
        &self,
        workspace_id: &str,
        app_id: &str,
        error: Option<&str>,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "UPDATE app_deployments SET last_error = $1, updated_at = $2 \
             WHERE workspace_id = $3 AND app_id = $4",
        )
        .bind(error)
        .bind(Utc::now().to_rfc3339())
        .bind(workspace_id)
        .bind(app_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn delete_app_deployment(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<AppDeployment, AuthFailure> {
        let update_guard = self.virtual_hosts_update.lock().await;
        let _ingress_update = self.service_ingresses_update.lock().await;
        let deployment = self.resolve_app_deployment(workspace_id, target).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM app_deployments WHERE workspace_id = $1 AND app_id = $2")
            .bind(workspace_id)
            .bind(&deployment.app_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM machine_services WHERE workspace_id = $1 AND service_id = $2")
            .bind(workspace_id)
            .bind(&deployment.service_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.service_ingresses
            .write()
            .await
            .retain(|_, resolved| resolved.ingress.service_id != deployment.service_id);
        drop(update_guard);
        self.refresh_virtual_network_hosts()
            .await
            .map_err(|error| {
                AuthFailure::internal("virtual_host_refresh_failed", format!("{error:#}"))
            })?;
        Ok(deployment)
    }

    pub async fn list_machine_services(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<MachineService>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT service_id, workspace_id, name, server_id, target_agent_id, target_host, target_port, \
             protocol, created_at, created_by, updated_at, updated_by \
             FROM machine_services WHERE workspace_id = $1 \
             ORDER BY lower(name), service_id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(machine_service_from_row).collect()
    }

    pub async fn resolve_machine_service(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<MachineService, AuthFailure> {
        let row = sqlx::query(
            "SELECT service_id, workspace_id, name, server_id, target_agent_id, target_host, target_port, \
             protocol, created_at, created_by, updated_at, updated_by \
             FROM machine_services WHERE workspace_id = $1 AND service_id = $2",
        )
        .bind(workspace_id)
        .bind(target)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let row = match row {
            Some(row) => row,
            None => sqlx::query(
                "SELECT service_id, workspace_id, name, server_id, target_agent_id, target_host, target_port, \
                 protocol, created_at, created_by, updated_at, updated_by \
                 FROM machine_services WHERE workspace_id = $1 AND lower(name) = lower($2)",
            )
            .bind(workspace_id)
            .bind(target.trim())
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?
            .ok_or_else(|| {
                AuthFailure::not_found("service_not_found", "machine service does not exist")
            })?,
        };
        machine_service_from_row(row)
    }

    pub async fn create_machine_service(
        &self,
        workspace_id: &str,
        actor: &str,
        request: CreateMachineServiceRequest,
    ) -> Result<MachineService, AuthFailure> {
        let name = validate_resource_name(&request.name, "service")?;
        let target_agent_id = request
            .target_agent_id
            .as_deref()
            .map(validate_service_target_agent_id)
            .transpose()?;
        let target_host = if target_agent_id.is_some() {
            validate_agent_service_target_host(&request.target_host)?
        } else {
            validate_service_target_host(&request.target_host)?
        };
        if request.target_port == 0 {
            return Err(AuthFailure::bad_request(
                "invalid_service",
                "target_port must be between 1 and 65535",
            ));
        }
        let now = Utc::now();
        let service = MachineService {
            service_id: format!("svc_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            name,
            server_id: request.server_id,
            target_agent_id,
            target_host,
            target_port: request.target_port,
            protocol: request.protocol,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        sqlx::query(
            "INSERT INTO machine_services(\
             service_id, workspace_id, name, server_id, target_agent_id, target_host, target_port, protocol, \
             created_at, created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(&service.service_id)
        .bind(&service.workspace_id)
        .bind(&service.name)
        .bind(&service.server_id)
        .bind(&service.target_agent_id)
        .bind(&service.target_host)
        .bind(i64::from(service.target_port))
        .bind(machine_service_protocol_str(service.protocol))
        .bind(service.created_at.to_rfc3339())
        .bind(&service.created_by)
        .bind(service.updated_at.to_rfc3339())
        .bind(&service.updated_by)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("service_exists", "service name already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        Ok(service)
    }

    pub async fn update_machine_service(
        &self,
        workspace_id: &str,
        target: &str,
        actor: &str,
        request: UpdateMachineServiceRequest,
    ) -> Result<MachineService, AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let current = self.resolve_machine_service(workspace_id, target).await?;
        if current.target_agent_id.is_some() {
            if request
                .server_id
                .as_ref()
                .is_some_and(|server_id| server_id != &current.server_id)
            {
                return Err(AuthFailure::bad_request(
                    "agent_service_scope_immutable",
                    "an Agent service cannot be moved to another machine",
                ));
            }
            if let Some(host) = request.target_host.as_deref() {
                validate_agent_service_target_host(host)?;
            }
        }
        let service = MachineService {
            name: request
                .name
                .as_deref()
                .map(|name| validate_resource_name(name, "service"))
                .transpose()?
                .unwrap_or(current.name),
            server_id: request.server_id.unwrap_or(current.server_id),
            target_host: request
                .target_host
                .as_deref()
                .map(validate_service_target_host)
                .transpose()?
                .unwrap_or(current.target_host),
            target_port: request.target_port.unwrap_or(current.target_port),
            protocol: request.protocol.unwrap_or(current.protocol),
            updated_at: Utc::now(),
            updated_by: actor.to_string(),
            ..current
        };
        if service.target_port == 0 {
            return Err(AuthFailure::bad_request(
                "invalid_service",
                "target_port must be between 1 and 65535",
            ));
        }
        sqlx::query(
            "UPDATE machine_services SET name = $1, server_id = $2, target_host = $3, \
             target_port = $4, protocol = $5, updated_at = $6, updated_by = $7 \
             WHERE workspace_id = $8 AND service_id = $9",
        )
        .bind(&service.name)
        .bind(&service.server_id)
        .bind(&service.target_host)
        .bind(i64::from(service.target_port))
        .bind(machine_service_protocol_str(service.protocol))
        .bind(service.updated_at.to_rfc3339())
        .bind(&service.updated_by)
        .bind(workspace_id)
        .bind(&service.service_id)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("service_exists", "service name already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        if let Some(hosts) = self.virtual_hosts.write().await.get_mut(workspace_id) {
            for host in hosts
                .values_mut()
                .filter(|host| host.service_id == service.service_id)
            {
                host.service_protocol = service.protocol;
                host.destination_server_id.clone_from(&service.server_id);
                host.destination_agent_id
                    .clone_from(&service.target_agent_id);
                host.target_host.clone_from(&service.target_host);
                host.target_port = Some(service.target_port);
            }
        }
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(service)
    }

    pub async fn delete_machine_service(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<MachineService, AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let service = self.resolve_machine_service(workspace_id, target).await?;
        sqlx::query("DELETE FROM machine_services WHERE workspace_id = $1 AND service_id = $2")
            .bind(workspace_id)
            .bind(&service.service_id)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        if let Some(hosts) = self.virtual_hosts.write().await.get_mut(workspace_id) {
            hosts.retain(|_, host| host.service_id != service.service_id);
        }
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(service)
    }

    pub async fn list_virtual_network_hosts(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<VirtualNetworkHost>, AuthFailure> {
        let mut hosts = self
            .virtual_hosts
            .read()
            .await
            .get(workspace_id)
            .map(|hosts| hosts.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        hosts.sort_by(|left, right| left.hostname.cmp(&right.hostname));
        Ok(hosts)
    }

    pub async fn resolve_virtual_network_host(
        &self,
        workspace_id: &str,
        hostname: &str,
    ) -> Result<Option<VirtualNetworkHost>, AuthFailure> {
        let hostname = match normalize_virtual_hostname(hostname) {
            Ok(hostname) => hostname,
            Err(_) => return Ok(None),
        };
        Ok(self
            .virtual_hosts
            .read()
            .await
            .get(workspace_id)
            .and_then(|hosts| hosts.get(&hostname))
            .cloned())
    }

    pub async fn create_virtual_network_host(
        &self,
        workspace_id: &str,
        created_by: &str,
        request: CreateVirtualNetworkHostRequest,
    ) -> Result<VirtualNetworkHost, AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let hostname = normalize_virtual_hostname(&request.hostname)?;
        let service = self
            .resolve_machine_service(workspace_id, &request.service_id)
            .await?;
        let record = VirtualNetworkHost {
            workspace_id: workspace_id.to_string(),
            hostname,
            service_id: service.service_id,
            service_protocol: service.protocol,
            destination_server_id: service.server_id,
            destination_agent_id: service.target_agent_id,
            target_host: service.target_host,
            target_port: Some(service.target_port),
            created_at: Utc::now(),
            created_by: created_by.to_string(),
        };
        let result = sqlx::query(
            "INSERT INTO virtual_network_hosts(\
             workspace_id, hostname, service_id, created_at, created_by) VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&record.workspace_id)
        .bind(&record.hostname)
        .bind(&record.service_id)
        .bind(record.created_at.to_rfc3339())
        .bind(&record.created_by)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => {
                self.virtual_hosts
                    .write()
                    .await
                    .entry(workspace_id.to_string())
                    .or_default()
                    .insert(record.hostname.clone(), record.clone());
                self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
                Ok(record)
            }
            Err(error)
                if error
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation()) =>
            {
                Err(AuthFailure::conflict(
                    "virtual_host_exists",
                    "virtual hostname already exists in this workspace",
                ))
            }
            Err(error) => Err(AuthFailure::database(error)),
        }
    }

    pub async fn delete_virtual_network_host(
        &self,
        workspace_id: &str,
        hostname: &str,
    ) -> Result<(), AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let hostname = normalize_virtual_hostname(hostname)?;
        let result = sqlx::query(
            "DELETE FROM virtual_network_hosts WHERE workspace_id = $1 AND hostname = $2",
        )
        .bind(workspace_id)
        .bind(&hostname)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() == 0 {
            return Err(AuthFailure::not_found(
                "virtual_host_not_found",
                "virtual host does not exist",
            ));
        }
        if let Some(hosts) = self.virtual_hosts.write().await.get_mut(workspace_id) {
            hosts.remove(&hostname);
        }
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub async fn refresh_virtual_network_hosts(&self) -> anyhow::Result<()> {
        let _update = self.virtual_hosts_update.lock().await;
        let rows = sqlx::query(
            "SELECT v.workspace_id, v.hostname, v.service_id, s.protocol AS service_protocol, \
             s.server_id AS destination_server_id, s.target_agent_id AS destination_agent_id, \
             s.target_host, s.target_port, v.created_at, v.created_by \
             FROM virtual_network_hosts v \
             JOIN machine_services s ON s.service_id = v.service_id \
             JOIN workspaces w ON w.workspace_id = v.workspace_id \
             WHERE w.deleted_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut refreshed = HashMap::<String, HashMap<String, VirtualNetworkHost>>::new();
        for row in rows {
            let host = virtual_network_host_from_row(row)
                .map_err(|error| anyhow::anyhow!(error.into_parts().1.message))?;
            refreshed
                .entry(host.workspace_id.clone())
                .or_default()
                .insert(host.hostname.clone(), host);
        }
        *self.virtual_hosts.write().await = refreshed;
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub async fn virtual_network_hosts_snapshot(
        &self,
        workspace_id: &str,
    ) -> Result<treer_protocol::VirtualNetworkHostsSnapshot, AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let mut hosts = self
            .virtual_hosts
            .read()
            .await
            .get(workspace_id)
            .map(|hosts| hosts.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        hosts.sort_by(|left, right| left.hostname.cmp(&right.hostname));
        Ok(treer_protocol::VirtualNetworkHostsSnapshot {
            workspace_id: workspace_id.to_string(),
            revision: self.virtual_hosts_revision.load(Ordering::SeqCst),
            hosts,
        })
    }

    pub async fn list_service_ingresses(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<ServiceIngress>, AuthFailure> {
        let mut ingresses = self
            .service_ingresses
            .read()
            .await
            .values()
            .filter(|resolved| resolved.ingress.workspace_id == workspace_id)
            .map(|resolved| resolved.ingress.clone())
            .collect::<Vec<_>>();
        ingresses.sort_by(|left, right| left.hostname.cmp(&right.hostname));
        Ok(ingresses)
    }

    pub async fn resolve_service_ingress(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<ResolvedServiceIngress, AuthFailure> {
        self.service_ingresses
            .read()
            .await
            .values()
            .find(|resolved| {
                resolved.ingress.workspace_id == workspace_id
                    && (resolved.ingress.ingress_id == target
                        || resolved.ingress.hostname.eq_ignore_ascii_case(target))
            })
            .cloned()
            .ok_or_else(|| {
                AuthFailure::not_found("ingress_not_found", "service ingress does not exist")
            })
    }

    pub async fn resolve_service_ingress_hostname(
        &self,
        hostname: &str,
    ) -> Result<Option<ResolvedServiceIngress>, AuthFailure> {
        if let Some(resolved) = self
            .service_ingresses
            .read()
            .await
            .get(&hostname.to_ascii_lowercase())
            .cloned()
        {
            return Ok(Some(resolved));
        }
        let row = sqlx::query(
            "SELECT i.ingress_id, i.workspace_id, i.service_id, i.hostname, i.access, i.enabled, \
             i.created_at, i.created_by, i.updated_at, i.updated_by, \
             s.name AS service_name, s.server_id, s.target_agent_id, s.target_host, s.target_port, \
             s.protocol AS service_protocol, s.created_at AS service_created_at, \
             s.created_by AS service_created_by, s.updated_at AS service_updated_at, \
             s.updated_by AS service_updated_by \
             FROM service_ingresses i JOIN machine_services s ON s.service_id = i.service_id \
             JOIN workspaces w ON w.workspace_id = i.workspace_id \
             WHERE lower(i.hostname) = lower($1) AND w.deleted_at IS NULL",
        )
        .bind(hostname)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let Some(row) = row else { return Ok(None) };
        let resolved = resolved_service_ingress_from_row(row)?;
        self.service_ingresses.write().await.insert(
            resolved.ingress.hostname.to_ascii_lowercase(),
            resolved.clone(),
        );
        Ok(Some(resolved))
    }

    pub async fn create_service_ingress(
        &self,
        workspace_id: &str,
        actor: &str,
        base_domain: &str,
        request: CreateServiceIngressRequest,
    ) -> Result<ServiceIngress, AuthFailure> {
        let _update = self.service_ingresses_update.lock().await;
        let service = self
            .resolve_machine_service(workspace_id, &request.service_id)
            .await?;
        if service.protocol != MachineServiceProtocol::Http {
            return Err(AuthFailure::bad_request(
                "service_protocol_mismatch",
                "public ingress requires an HTTP service",
            ));
        }
        let slug = normalize_ingress_slug(request.slug.as_deref().unwrap_or(&service.name))?;
        let suffix = Uuid::new_v4().simple().to_string();
        let hostname = format!("{slug}-{}.{}", &suffix[..8], base_domain);
        if hostname.len() > 253 {
            return Err(AuthFailure::bad_request(
                "invalid_ingress_slug",
                "generated ingress hostname is too long",
            ));
        }
        let now = Utc::now();
        let ingress = ServiceIngress {
            ingress_id: format!("ing_{}", Uuid::new_v4().simple()),
            workspace_id: workspace_id.to_string(),
            service_id: service.service_id.clone(),
            hostname: hostname.clone(),
            access: request.access,
            enabled: true,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        sqlx::query(
            "INSERT INTO service_ingresses(\
             ingress_id, workspace_id, service_id, hostname, access, enabled, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&ingress.ingress_id)
        .bind(&ingress.workspace_id)
        .bind(&ingress.service_id)
        .bind(&ingress.hostname)
        .bind(service_ingress_access_str(ingress.access))
        .bind(ingress.enabled)
        .bind(ingress.created_at.to_rfc3339())
        .bind(&ingress.created_by)
        .bind(ingress.updated_at.to_rfc3339())
        .bind(&ingress.updated_by)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("ingress_exists", "service ingress hostname already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        self.service_ingresses.write().await.insert(
            hostname.to_ascii_lowercase(),
            ResolvedServiceIngress {
                ingress: ingress.clone(),
                service,
            },
        );
        Ok(ingress)
    }

    pub async fn ensure_app_ingress(
        &self,
        app: &AppDeployment,
        actor: &str,
        base_domain: &str,
    ) -> Result<ServiceIngress, AuthFailure> {
        let service = self
            .resolve_machine_service(&app.workspace_id, &app.service_id)
            .await?;
        let _update = self.service_ingresses_update.lock().await;
        let hostname = managed_app_ingress_hostname(&app.name, &app.app_id, base_domain)?;
        if let Some(existing) = self.service_ingresses.read().await.get(&hostname) {
            return if existing.ingress.service_id == app.service_id
                && existing.ingress.access == ServiceIngressAccess::Workspace
            {
                Ok(existing.ingress.clone())
            } else {
                Err(AuthFailure::conflict(
                    "ingress_exists",
                    "generated App ingress hostname already exists",
                ))
            };
        }
        let now = Utc::now();
        let ingress = ServiceIngress {
            ingress_id: format!("ing_{}", Uuid::new_v4().simple()),
            workspace_id: app.workspace_id.clone(),
            service_id: app.service_id.clone(),
            hostname: hostname.clone(),
            access: ServiceIngressAccess::Workspace,
            enabled: true,
            created_at: now,
            created_by: actor.to_string(),
            updated_at: now,
            updated_by: actor.to_string(),
        };
        let inserted = sqlx::query(
            "INSERT INTO service_ingresses(\
             ingress_id, workspace_id, service_id, hostname, access, enabled, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, 'workspace', TRUE, $5, $6, $7, $8) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&ingress.ingress_id)
        .bind(&ingress.workspace_id)
        .bind(&ingress.service_id)
        .bind(&ingress.hostname)
        .bind(ingress.created_at.to_rfc3339())
        .bind(&ingress.created_by)
        .bind(ingress.updated_at.to_rfc3339())
        .bind(&ingress.updated_by)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if inserted.rows_affected() == 0 {
            return self
                .resolve_service_ingress_hostname(&hostname)
                .await?
                .filter(|resolved| {
                    resolved.ingress.service_id == app.service_id
                        && resolved.ingress.access == ServiceIngressAccess::Workspace
                })
                .map(|resolved| resolved.ingress)
                .ok_or_else(|| {
                    AuthFailure::conflict(
                        "ingress_exists",
                        "generated App ingress hostname already exists",
                    )
                });
        }
        self.service_ingresses.write().await.insert(
            hostname,
            ResolvedServiceIngress {
                ingress: ingress.clone(),
                service,
            },
        );
        Ok(ingress)
    }

    pub async fn ensure_managed_app_ingresses(
        &self,
        actor: &str,
        base_domain: &str,
    ) -> Result<usize, AuthFailure> {
        let rows = sqlx::query(
            "SELECT a.app_id, a.workspace_id, a.name, a.server_id, a.command, a.args, a.cwd, \
             a.port, a.hostname, a.service_id, a.desired_state, a.runtime_agent_id, \
             a.restart_count, a.last_error, a.created_at, a.created_by, a.updated_at, a.updated_by \
             FROM app_deployments a JOIN workspaces w ON w.workspace_id = a.workspace_id \
             WHERE w.deleted_at IS NULL ORDER BY a.app_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let apps = rows
            .into_iter()
            .map(app_deployment_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let mut created = 0;
        for app in apps {
            let hostname = managed_app_ingress_hostname(&app.name, &app.app_id, base_domain)?;
            let existed = self.service_ingresses.read().await.contains_key(&hostname);
            self.ensure_app_ingress(&app, actor, base_domain).await?;
            created += usize::from(!existed);
        }
        Ok(created)
    }

    pub async fn update_service_ingress(
        &self,
        workspace_id: &str,
        target: &str,
        actor: &str,
        request: UpdateServiceIngressRequest,
    ) -> Result<ServiceIngress, AuthFailure> {
        let _update = self.service_ingresses_update.lock().await;
        let current = self.resolve_service_ingress(workspace_id, target).await?;
        let mut ingress = current.ingress;
        ingress.access = request.access.unwrap_or(ingress.access);
        ingress.enabled = request.enabled.unwrap_or(ingress.enabled);
        ingress.updated_at = Utc::now();
        ingress.updated_by = actor.to_string();
        sqlx::query(
            "UPDATE service_ingresses SET access = $1, enabled = $2, updated_at = $3, \
             updated_by = $4 WHERE workspace_id = $5 AND ingress_id = $6",
        )
        .bind(service_ingress_access_str(ingress.access))
        .bind(ingress.enabled)
        .bind(ingress.updated_at.to_rfc3339())
        .bind(&ingress.updated_by)
        .bind(workspace_id)
        .bind(&ingress.ingress_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        self.service_ingresses.write().await.insert(
            ingress.hostname.to_ascii_lowercase(),
            ResolvedServiceIngress {
                ingress: ingress.clone(),
                service: current.service,
            },
        );
        Ok(ingress)
    }

    pub async fn delete_service_ingress(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<ServiceIngress, AuthFailure> {
        let _update = self.service_ingresses_update.lock().await;
        let resolved = self.resolve_service_ingress(workspace_id, target).await?;
        sqlx::query("DELETE FROM service_ingresses WHERE workspace_id = $1 AND ingress_id = $2")
            .bind(workspace_id)
            .bind(&resolved.ingress.ingress_id)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        self.service_ingresses
            .write()
            .await
            .remove(&resolved.ingress.hostname.to_ascii_lowercase());
        Ok(resolved.ingress)
    }

    pub async fn refresh_service_ingresses(&self) -> anyhow::Result<()> {
        let _update = self.service_ingresses_update.lock().await;
        let rows = sqlx::query(
            "SELECT i.ingress_id, i.workspace_id, i.service_id, i.hostname, i.access, i.enabled, \
             i.created_at, i.created_by, i.updated_at, i.updated_by, \
             s.name AS service_name, s.server_id, s.target_agent_id, s.target_host, s.target_port, \
             s.protocol AS service_protocol, s.created_at AS service_created_at, \
             s.created_by AS service_created_by, s.updated_at AS service_updated_at, \
             s.updated_by AS service_updated_by \
             FROM service_ingresses i JOIN machine_services s ON s.service_id = i.service_id \
             JOIN workspaces w ON w.workspace_id = i.workspace_id \
             WHERE w.deleted_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut refreshed = HashMap::new();
        for row in rows {
            let resolved = resolved_service_ingress_from_row(row)
                .map_err(|error| anyhow::anyhow!(error.into_parts().1.message))?;
            refreshed.insert(resolved.ingress.hostname.to_ascii_lowercase(), resolved);
        }
        *self.service_ingresses.write().await = refreshed;
        Ok(())
    }

    pub async fn create_ingress_auth_code(
        &self,
        ingress: &ServiceIngress,
        user_id: &str,
        return_path: &str,
    ) -> Result<String, AuthFailure> {
        self.require_workspace_member(&ingress.workspace_id, user_id)
            .await?;
        let return_path = validate_ingress_return_path(return_path)?;
        let code = format!("iac_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        sqlx::query("DELETE FROM ingress_auth_codes WHERE expires_at <= $1 OR used_at IS NOT NULL")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO ingress_auth_codes(code, ingress_id, user_id, return_path, expires_at) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&code)
        .bind(&ingress.ingress_id)
        .bind(user_id)
        .bind(return_path)
        .bind((now + Duration::minutes(INGRESS_AUTH_CODE_TTL_MINUTES)).to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(code)
    }

    pub async fn consume_ingress_auth_code(
        &self,
        hostname: &str,
        code: &str,
    ) -> Result<ConsumedIngressAuthorization, AuthFailure> {
        let now = Utc::now();
        let row = sqlx::query(
            "SELECT c.ingress_id, c.user_id, c.return_path, i.workspace_id \
             FROM ingress_auth_codes c JOIN service_ingresses i ON i.ingress_id = c.ingress_id \
             WHERE c.code = $1 AND lower(i.hostname) = lower($2) AND i.enabled = TRUE \
             AND c.used_at IS NULL AND c.expires_at > $3",
        )
        .bind(code)
        .bind(hostname)
        .bind(now.to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_ingress_authorization)?;
        let workspace_id: String = row.get("workspace_id");
        let user_id: String = row.get("user_id");
        self.require_workspace_member(&workspace_id, &user_id)
            .await?;
        let result = sqlx::query(
            "UPDATE ingress_auth_codes SET used_at = $1 WHERE code = $2 AND used_at IS NULL",
        )
        .bind(now.to_rfc3339())
        .bind(code)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(invalid_ingress_authorization());
        }
        let session_token = format!("ias_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        sqlx::query("DELETE FROM ingress_sessions WHERE expires_at <= $1")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO ingress_sessions(token, ingress_id, user_id, created_at, expires_at) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&session_token)
        .bind(row.get::<String, _>("ingress_id"))
        .bind(&user_id)
        .bind(now.to_rfc3339())
        .bind((now + Duration::hours(INGRESS_SESSION_TTL_HOURS)).to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(ConsumedIngressAuthorization {
            session_token,
            return_path: row.get("return_path"),
        })
    }

    pub async fn authenticate_ingress_session(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Option<String>, AuthFailure> {
        let row = sqlx::query(
            "SELECT s.user_id, i.workspace_id FROM ingress_sessions s \
             JOIN service_ingresses i ON i.ingress_id = s.ingress_id \
             WHERE s.token = $1 AND lower(i.hostname) = lower($2) AND i.enabled = TRUE \
             AND s.expires_at > $3",
        )
        .bind(token)
        .bind(hostname)
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let Some(row) = row else { return Ok(None) };
        let user_id: String = row.get("user_id");
        let workspace_id: String = row.get("workspace_id");
        if self
            .require_workspace_member(&workspace_id, &user_id)
            .await
            .is_err()
        {
            return Ok(None);
        }
        Ok(Some(user_id))
    }

    pub async fn create_app_oauth_code(
        &self,
        grant: &AppOAuthGrant,
        redirect_uri: &str,
        code_challenge: &str,
    ) -> Result<String, AuthFailure> {
        if !valid_pkce_challenge(code_challenge) {
            return Err(AuthFailure::bad_request(
                "invalid_code_challenge",
                "app OAuth requires a valid S256 PKCE code challenge",
            ));
        }
        if !matches!(grant.role.as_str(), "owner" | "admin" | "member") {
            return Err(AuthFailure::bad_request(
                "invalid_app_identity",
                "app OAuth role is invalid",
            ));
        }
        let code = format!("aoc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM app_oauth_codes WHERE expires_at <= $1 OR used_at IS NOT NULL")
            .bind(now.to_rfc3339())
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO app_oauth_codes(\
             code_hash, workspace_id, service_id, user_id, preferred_name, role, redirect_uri, \
             code_challenge, created_at, expires_at) \
             VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(fast_secret_hash(&code))
        .bind(&grant.workspace_id)
        .bind(&grant.service_id)
        .bind(&grant.user_id)
        .bind(&grant.preferred_name)
        .bind(&grant.role)
        .bind(redirect_uri)
        .bind(code_challenge)
        .bind(now.to_rfc3339())
        .bind((now + Duration::minutes(APP_OAUTH_CODE_TTL_MINUTES)).to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(code)
    }

    pub async fn consume_app_oauth_code(
        &self,
        code: &str,
        service_id: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<AppOAuthGrant, AuthFailure> {
        if !valid_pkce_verifier(code_verifier) {
            return Err(invalid_app_oauth_code());
        }
        let now = Utc::now();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT workspace_id, service_id, user_id, preferred_name, role, code_challenge \
             FROM app_oauth_codes WHERE code_hash = $1 AND service_id = $2 \
             AND redirect_uri = $3 AND used_at IS NULL AND expires_at > $4 FOR UPDATE",
        )
        .bind(fast_secret_hash(code))
        .bind(service_id)
        .bind(redirect_uri)
        .bind(now.to_rfc3339())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_app_oauth_code)?;
        let expected_challenge: String = row.get("code_challenge");
        let actual_challenge = pkce_challenge(code_verifier);
        if actual_challenge.len() != expected_challenge.len()
            || actual_challenge
                .as_bytes()
                .ct_eq(expected_challenge.as_bytes())
                .unwrap_u8()
                != 1
        {
            return Err(invalid_app_oauth_code());
        }
        let result = sqlx::query(
            "UPDATE app_oauth_codes SET used_at = $1 \
             WHERE code_hash = $2 AND used_at IS NULL",
        )
        .bind(now.to_rfc3339())
        .bind(fast_secret_hash(code))
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(invalid_app_oauth_code());
        }
        let grant = AppOAuthGrant {
            workspace_id: row.get("workspace_id"),
            service_id: row.get("service_id"),
            user_id: row.get("user_id"),
            preferred_name: row.get("preferred_name"),
            role: row.get("role"),
        };
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.require_workspace_member(&grant.workspace_id, &grant.user_id)
            .await?;
        Ok(grant)
    }

    pub(crate) fn authentication_disabled(&self) -> bool {
        self.disabled
    }

    async fn membership_role(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<Option<String>, AuthFailure> {
        if self.disabled {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM organizations WHERE organization_id = $1",
            )
            .bind(organization_id)
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            return Ok((exists != 0).then(|| "owner".to_string()));
        }
        sqlx::query_scalar(
            "SELECT role FROM organization_members \
             WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)
    }

    async fn require_manager(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<String, AuthFailure> {
        let role = self
            .require_organization_member(organization_id, user_id)
            .await?;
        if matches!(role.as_str(), "owner" | "admin") {
            Ok(role)
        } else {
            Err(AuthFailure::forbidden(
                "organization_manager_required",
                "organization owner or administrator access required",
            ))
        }
    }

    pub async fn set_machine_name(
        &self,
        workspace_id: &str,
        server_id: &str,
        name: &str,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4) \
             ON CONFLICT(server_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, \
             updated_at = excluded.updated_at",
        )
        .bind(server_id)
        .bind(workspace_id)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn set_agent_name(
        &self,
        workspace_id: &str,
        agent_id: &str,
        name: &str,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "INSERT INTO agent_names(agent_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4) \
             ON CONFLICT(agent_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, \
             updated_at = excluded.updated_at",
        )
        .bind(agent_id)
        .bind(workspace_id)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn apply_server_name(&self, server: &mut ServerInfo) -> Result<(), AuthFailure> {
        if server.name.trim().is_empty() {
            server.name.clone_from(&server.hostname);
        }
        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM machine_names WHERE server_id = $1 AND workspace_id = $2",
        )
        .bind(&server.server_id)
        .bind(&server.workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if let Some(name) = name {
            server.name = name;
        }
        Ok(())
    }

    pub async fn apply_agent_names(
        &self,
        snapshot: &mut AgentServerSnapshot,
    ) -> Result<Vec<String>, AuthFailure> {
        let rows = sqlx::query("SELECT agent_id, name FROM agent_names WHERE workspace_id = $1")
            .bind(&snapshot.server.workspace_id)
            .fetch_all(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        let names: HashMap<String, String> = rows
            .into_iter()
            .map(|row| (row.get("agent_id"), row.get("name")))
            .collect();
        for agent in &mut snapshot.agents {
            if let Some(name) = names.get(&agent.agent_id) {
                agent.name.clone_from(name);
            }
        }
        let deleted = sqlx::query_scalar::<_, String>(
            "SELECT agent_id FROM deleted_agents WHERE workspace_id = $1",
        )
        .bind(&snapshot.server.workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let deleted_set: std::collections::HashSet<_> =
            deleted.iter().map(String::as_str).collect();
        snapshot
            .agents
            .retain(|agent| !deleted_set.contains(agent.agent_id.as_str()));
        Ok(deleted)
    }

    pub async fn delete_agent(
        &self,
        workspace_id: &str,
        agent_id: &str,
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO deleted_agents(agent_id, workspace_id, deleted_at) \
             VALUES($1, $2, $3) \
             ON CONFLICT(agent_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, deleted_at = excluded.deleted_at",
        )
        .bind(agent_id)
        .bind(workspace_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM agent_names WHERE agent_id = $1 AND workspace_id = $2")
            .bind(agent_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "DELETE FROM machine_services WHERE workspace_id = $1 AND target_agent_id = $2",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE agent_credentials SET revoked_at = $1 \
             WHERE agent_id = $2 AND workspace_id = $3 AND revoked_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(agent_id)
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.agent_credentials.write().await.remove(agent_id);
        Ok(())
    }

    pub async fn delete_machine(
        &self,
        workspace_id: &str,
        server_id: &str,
        agent_ids: &[String],
    ) -> Result<(), AuthFailure> {
        let _update = self.virtual_hosts_update.lock().await;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        if !self.disabled {
            let update = sqlx::query(
                "UPDATE machines SET revoked_at = $1 \
                 WHERE server_id = $2 AND workspace_id = $3 AND revoked_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(server_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            if update.rows_affected() != 1 {
                return Err(AuthFailure::conflict(
                    "machine_already_deleted",
                    "machine credential is already revoked",
                ));
            }
        }
        sqlx::query("DELETE FROM machine_names WHERE server_id = $1 AND workspace_id = $2")
            .bind(server_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM machine_services WHERE workspace_id = $1 AND server_id = $2")
            .bind(workspace_id)
            .bind(server_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        for agent_id in agent_ids {
            sqlx::query("DELETE FROM agent_names WHERE agent_id = $1 AND workspace_id = $2")
                .bind(agent_id)
                .bind(workspace_id)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
            sqlx::query("DELETE FROM deleted_agents WHERE agent_id = $1 AND workspace_id = $2")
                .bind(agent_id)
                .bind(workspace_id)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
            sqlx::query(
                "UPDATE agent_credentials SET revoked_at = $1 \
                 WHERE agent_id = $2 AND workspace_id = $3 AND revoked_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(agent_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        transaction.commit().await.map_err(AuthFailure::database)?;
        let mut credentials = self.agent_credentials.write().await;
        for agent_id in agent_ids {
            credentials.remove(agent_id);
        }
        drop(credentials);
        if let Some(hosts) = self.virtual_hosts.write().await.get_mut(workspace_id) {
            hosts.retain(|_, host| host.destination_server_id != server_id);
        }
        self.virtual_hosts_revision.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub async fn create_machine_enrollment(
        &self,
        workspace_id: &str,
        created_by: &str,
    ) -> Result<String, AuthFailure> {
        let enrollment_id = Uuid::new_v4().simple().to_string();
        let secret = random_secret();
        let enrollment = format_machine_enrollment_key(workspace_id, &enrollment_id, &secret)
            .map_err(|error| AuthFailure::internal("machine_enrollment_error", error.message))?;
        let identifier = enrollment
            .split_once('.')
            .map(|(identifier, _)| identifier)
            .ok_or_else(invalid_machine_enrollment)?;
        let secret_hash = hash_password(&secret)?;
        let now = Utc::now();
        let expires_at = now + Duration::minutes(MACHINE_ENROLLMENT_TTL_MINUTES);
        sqlx::query(
            "INSERT INTO machine_enrollments(\
             enrollment_id, workspace_id, secret_hash, created_at, expires_at, created_by) \
             VALUES($1, $2, $3, $4, $5, $6)",
        )
        .bind(identifier)
        .bind(workspace_id)
        .bind(secret_hash)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .bind(created_by)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(enrollment)
    }

    #[cfg(test)]
    pub async fn claim_machine_enrollment(
        &self,
        token: &str,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        self.claim_machine_enrollment_for_installation(token, None, None, None)
            .await
    }

    pub async fn claim_machine_enrollment_for_installation(
        &self,
        token: &str,
        installation_id: Option<&str>,
        machine_name: Option<&str>,
        existing_server_id: Option<&str>,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        let installation_id = installation_id.map(validate_installation_id).transpose()?;
        let machine_name = machine_name
            .map(|name| validate_resource_name(name, "machine"))
            .transpose()?;
        let existing_server_id = existing_server_id
            .map(validate_machine_server_id)
            .transpose()?;
        if existing_server_id.is_some() && installation_id.is_none() {
            return Err(AuthFailure::bad_request(
                "invalid_machine_identity",
                "an installed machine ID requires an installation identity",
            ));
        }
        let enrollment =
            parse_machine_enrollment_key(token).map_err(|_| invalid_machine_enrollment())?;
        let now = Utc::now();
        let row = sqlx::query(
            "SELECT workspace_id, secret_hash, created_by \
             FROM machine_enrollments \
             WHERE enrollment_id = $1 AND used_at IS NULL AND expires_at > $2",
        )
        .bind(&enrollment.identifier)
        .bind(now.to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_machine_enrollment)?;
        let secret_hash: String = row.get("secret_hash");
        if !verify_password(&enrollment.secret, &secret_hash) {
            return Err(invalid_machine_enrollment());
        }
        let workspace_id: String = row.get("workspace_id");
        if workspace_id != enrollment.workspace_id {
            return Err(invalid_machine_enrollment());
        }
        let created_by: String = row.get("created_by");
        let machine_secret = random_secret();
        let machine_secret_hash = hash_password(&machine_secret)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let workspace =
            sqlx::query("SELECT deleted_at FROM workspaces WHERE workspace_id = $1 FOR KEY SHARE")
                .bind(&workspace_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
        if workspace.is_some_and(|row| row.get::<Option<String>, _>("deleted_at").is_some()) {
            return Err(invalid_machine_enrollment());
        }
        let server_for_installation = if let Some(installation_id) = installation_id.as_deref() {
            sqlx::query_scalar::<_, String>(
                "SELECT server_id FROM machines WHERE workspace_id = $1 AND installation_id = $2",
            )
            .bind(&workspace_id)
            .bind(installation_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?
        } else {
            None
        };
        let server_id = match (
            server_for_installation.as_deref(),
            existing_server_id.as_deref(),
        ) {
            (Some(server_id), Some(expected)) if server_id != expected => {
                return Err(AuthFailure::conflict(
                    "machine_identity_conflict",
                    "this installation identity is already bound to another machine",
                ));
            }
            (Some(server_id), _) => server_id.to_string(),
            (None, Some(server_id)) => {
                let existing = sqlx::query(
                    "SELECT workspace_id, installation_id FROM machines WHERE server_id = $1",
                )
                .bind(server_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
                if let Some(existing) = existing {
                    let existing_workspace: String = existing.get("workspace_id");
                    let existing_installation: Option<String> = existing.get("installation_id");
                    if existing_workspace != workspace_id
                        || existing_installation
                            .as_deref()
                            .is_some_and(|value| Some(value) != installation_id.as_deref())
                    {
                        return Err(AuthFailure::conflict(
                            "machine_identity_conflict",
                            "the installed machine is bound to another workspace or installation identity",
                        ));
                    }
                }
                server_id.to_string()
            }
            (None, None) => format!("srv_{}", Uuid::new_v4().simple()),
        };
        let update = sqlx::query(
            "UPDATE machine_enrollments SET used_at = $1, server_id = $2 \
             WHERE enrollment_id = $3 AND used_at IS NULL AND expires_at > $4",
        )
        .bind(now.to_rfc3339())
        .bind(&server_id)
        .bind(&enrollment.identifier)
        .bind(now.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if update.rows_affected() != 1 {
            return Err(invalid_machine_enrollment());
        }
        let existing_machine =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM machines WHERE server_id = $1")
                .bind(&server_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?
                != 0;
        if existing_machine {
            sqlx::query(
                "UPDATE machines SET installation_id = $1, secret_hash = $2, enrolled_by = $3, revoked_at = NULL \
                 WHERE server_id = $4 AND workspace_id = $5",
            )
            .bind(installation_id.as_deref())
            .bind(machine_secret_hash)
            .bind(&created_by)
            .bind(&server_id)
            .bind(&workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        } else {
            sqlx::query(
                "INSERT INTO machines(\
                 server_id, workspace_id, installation_id, secret_hash, created_at, enrolled_by) \
                 VALUES($1, $2, $3, $4, $5, $6)",
            )
            .bind(&server_id)
            .bind(&workspace_id)
            .bind(installation_id.as_deref())
            .bind(machine_secret_hash)
            .bind(now.to_rfc3339())
            .bind(&created_by)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        if let Some(machine_name) = machine_name {
            sqlx::query(
                "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
                 VALUES($1, $2, $3, $4) ON CONFLICT(server_id) DO UPDATE SET \
                 workspace_id = excluded.workspace_id, name = excluded.name, updated_at = excluded.updated_at",
            )
            .bind(&server_id)
            .bind(&workspace_id)
            .bind(machine_name)
            .bind(now.to_rfc3339())
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(MachineEnrollmentClaim {
            workspace_id,
            machine_token: format!("{server_id}.{machine_secret}"),
            server_id,
        })
    }

    pub async fn claim_machine_enrollment_from_headers(
        &self,
        headers: &HeaderMap,
        installation_id: Option<&str>,
        machine_name: Option<&str>,
        existing_server_id: Option<&str>,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        let token = bearer_token(headers).ok_or_else(invalid_machine_enrollment)?;
        self.claim_machine_enrollment_for_installation(
            token,
            installation_id,
            machine_name,
            existing_server_id,
        )
        .await
    }

    pub async fn bind_machine_identity(
        &self,
        workspace_id: &str,
        server_id: &str,
        installation_id: &str,
        machine_name: &str,
    ) -> Result<(), AuthFailure> {
        let installation_id = validate_installation_id(installation_id)?;
        let machine_name = validate_resource_name(machine_name, "machine")?;
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let update = sqlx::query(
            "UPDATE machines SET installation_id = $1 \
             WHERE workspace_id = $2 AND server_id = $3 AND revoked_at IS NULL \
             AND (installation_id IS NULL OR installation_id = $4)",
        )
        .bind(&installation_id)
        .bind(workspace_id)
        .bind(server_id)
        .bind(&installation_id)
        .execute(&mut *transaction)
        .await;
        match update {
            Ok(update) if update.rows_affected() == 1 => {}
            Ok(_) => {
                return Err(AuthFailure::conflict(
                    "machine_identity_conflict",
                    "this machine is already bound to another installation identity",
                ));
            }
            Err(error)
                if error
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation()) =>
            {
                return Err(AuthFailure::conflict(
                    "machine_identity_conflict",
                    "this installation identity is already bound to another machine",
                ));
            }
            Err(error) => return Err(AuthFailure::database(error)),
        }
        sqlx::query(
            "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4) ON CONFLICT(server_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, updated_at = excluded.updated_at",
        )
        .bind(server_id)
        .bind(workspace_id)
        .bind(machine_name)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)
    }

    pub async fn authenticate_machine(
        &self,
        headers: &HeaderMap,
    ) -> Result<MachineSession, AuthFailure> {
        if self.disabled {
            return Ok(MachineSession {
                server_id: None,
                workspace_id: None,
            });
        }
        let token = bearer_token(headers).ok_or_else(machine_auth_required)?;
        let (server_id, secret) = parse_credential(token, "srv_")?;
        let row = sqlx::query(
            "SELECT m.workspace_id, m.secret_hash FROM machines m \
             LEFT JOIN workspaces w ON w.workspace_id = m.workspace_id \
             WHERE m.server_id = $1 AND m.revoked_at IS NULL AND w.deleted_at IS NULL",
        )
        .bind(server_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(machine_auth_required)?;
        let secret_hash: String = row.get("secret_hash");
        if !verify_password(secret, &secret_hash) {
            return Err(machine_auth_required());
        }
        Ok(MachineSession {
            server_id: Some(server_id.to_string()),
            workspace_id: Some(row.get("workspace_id")),
        })
    }

    pub async fn create_agent_credential(
        &self,
        workspace_id: &str,
        server_id: &str,
        agent_id: &str,
    ) -> Result<String, AuthFailure> {
        let credential = format!("wlc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let record = AgentCredentialRecord {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            secret_hash: fast_secret_hash(&credential),
            cached_at: Instant::now(),
        };
        sqlx::query(
            "INSERT INTO agent_credentials(agent_id, workspace_id, server_id, secret_hash, created_at) \
             VALUES($1, $2, $3, $4, $5) ON CONFLICT(agent_id) DO NOTHING",
        )
        .bind(agent_id)
        .bind(workspace_id)
        .bind(server_id)
        .bind(&record.secret_hash)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)
        .and_then(|result| {
            if result.rows_affected() == 1 {
                Ok(())
            } else {
                Err(AuthFailure::conflict(
                    "agent_credential_exists",
                    "Agent credential already exists",
                ))
            }
        })?;
        self.agent_credentials
            .write()
            .await
            .insert(agent_id.to_string(), record);
        Ok(credential)
    }

    pub async fn authenticate_agent(
        &self,
        machine: &MachineSession,
        headers: &HeaderMap,
    ) -> Result<Option<AgentSession>, AuthFailure> {
        let agent_id = optional_header(headers, AGENT_ID_HEADER)?;
        let credential = optional_header(headers, WORKLOAD_CREDENTIAL_HEADER)?;
        let (agent_id, credential) = match (agent_id, credential) {
            (None, None) => return Ok(None),
            (Some(agent_id), Some(credential)) => (agent_id, credential),
            _ => {
                return Err(AuthFailure::unauthorized(
                    "agent_authentication_required",
                    "Agent ID and workload credential are both required",
                ))
            }
        };
        let cached = self.agent_credentials.read().await.get(agent_id).cloned();
        let record = if let Some(record) =
            cached.filter(|record| record.cached_at.elapsed() < AGENT_CREDENTIAL_CACHE_TTL)
        {
            record
        } else {
            let row = sqlx::query(
                "SELECT c.workspace_id, c.server_id, c.secret_hash FROM agent_credentials c \
                 LEFT JOIN workspaces w ON w.workspace_id = c.workspace_id \
                 WHERE c.agent_id = $1 AND c.revoked_at IS NULL AND w.deleted_at IS NULL",
            )
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            let Some(row) = row else {
                self.agent_credentials.write().await.remove(agent_id);
                return Err(agent_auth_required());
            };
            let record = AgentCredentialRecord {
                workspace_id: row.get("workspace_id"),
                server_id: row.get("server_id"),
                secret_hash: row.get("secret_hash"),
                cached_at: Instant::now(),
            };
            self.agent_credentials
                .write()
                .await
                .insert(agent_id.to_string(), record.clone());
            record
        };
        if !machine.allows_server(&record.workspace_id, &record.server_id)
            || !fast_secret_matches(credential, &record.secret_hash)
        {
            return Err(agent_auth_required());
        }
        Ok(Some(AgentSession {
            agent_id: agent_id.to_string(),
            server_id: record.server_id,
            workspace_id: record.workspace_id,
        }))
    }

    pub async fn machine_is_active(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<bool, AuthFailure> {
        if self.disabled {
            return Ok(true);
        }
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM machines \
             WHERE workspace_id = $1 AND server_id = $2 AND revoked_at IS NULL",
        )
        .bind(workspace_id)
        .bind(server_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(count == 1)
    }

    async fn login(&self, email: &str, password: &str) -> Result<CurrentSession, AuthFailure> {
        let identifier = email.trim().to_ascii_lowercase();
        if identifier.is_empty()
            || identifier.len() > 254
            || identifier
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(AuthFailure::unauthorized(
                "invalid_credentials",
                "invalid email or password",
            ));
        }
        let row = sqlx::query(
            "SELECT id, email, preferred_name, password_hash FROM users \
             WHERE lower(email) = lower($1)",
        )
        .bind(&identifier)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let Some(row) = row else {
            return Err(AuthFailure::unauthorized(
                "invalid_credentials",
                "invalid email or password",
            ));
        };
        let password_hash: String = row.get("password_hash");
        if !verify_password(password, &password_hash) {
            return Err(AuthFailure::unauthorized(
                "invalid_credentials",
                "invalid email or password",
            ));
        }
        self.create_session(row.get("id"), row.get("email"), row.get("preferred_name"))
            .await
    }

    async fn request_password_reset(&self, email: &str) -> Result<(), AuthFailure> {
        normalize_email(email)?;
        let Some(sender) = &self.email_sender else {
            tracing::warn!("password reset requested but CLOUDFLARE_API_TOKEN is not configured");
            return Ok(());
        };
        let Some(pending) = self.create_password_reset(email).await? else {
            return Ok(());
        };
        let sender = sender.clone();
        let auth = self.clone();
        tokio::spawn(async move {
            if let Err(error) = sender
                .send_password_reset(&pending.recipient, &pending.url)
                .await
            {
                tracing::error!(%error, "failed to send password reset email");
                if let Err(error) = auth.revoke_password_reset(&pending.token_id).await {
                    tracing::error!(?error, "failed to revoke undelivered password reset token");
                }
            }
        });
        Ok(())
    }

    pub(crate) async fn create_password_reset(
        &self,
        email: &str,
    ) -> Result<Option<PendingPasswordReset>, AuthFailure> {
        let email = normalize_email(email)?;
        let token_id = format!("pwd_{}", Uuid::new_v4().simple());
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let secret_hash = hash_password(&secret)?;
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "DELETE FROM password_reset_tokens WHERE expires_at <= $1 \
             OR (used_at IS NOT NULL AND created_at <= $2)",
        )
        .bind(&now_text)
        .bind((now - Duration::days(1)).to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let user =
            sqlx::query("SELECT id, email FROM users WHERE lower(email) = lower($1) FOR UPDATE")
                .bind(email)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
        let Some(user) = user else {
            transaction.commit().await.map_err(AuthFailure::database)?;
            return Ok(None);
        };
        let user_id: String = user.get("id");
        let recent = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM password_reset_tokens \
             WHERE user_id = $1 AND used_at IS NULL AND created_at > $2",
        )
        .bind(&user_id)
        .bind((now - Duration::seconds(PASSWORD_RESET_RATE_LIMIT_SECONDS)).to_rfc3339())
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if recent > 0 {
            transaction.commit().await.map_err(AuthFailure::database)?;
            return Ok(None);
        }
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 \
             WHERE user_id = $2 AND used_at IS NULL",
        )
        .bind(&now_text)
        .bind(&user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO password_reset_tokens(\
                 token_id, user_id, secret_hash, created_at, expires_at\
             ) VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&token_id)
        .bind(&user_id)
        .bind(secret_hash)
        .bind(&now_text)
        .bind((now + Duration::minutes(PASSWORD_RESET_TTL_MINUTES)).to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;

        let token = format!("{token_id}.{secret}");
        let mut url = self.app_public_url.clone();
        url.set_path("/");
        url.set_query(None);
        url.set_fragment(None);
        url.query_pairs_mut().append_pair("reset", &token);
        Ok(Some(PendingPasswordReset {
            token_id,
            recipient: user.get("email"),
            url,
        }))
    }

    async fn revoke_password_reset(&self, token_id: &str) -> Result<(), AuthFailure> {
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 \
             WHERE token_id = $2 AND used_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(token_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    async fn reset_password(&self, token: &str, password: &str) -> Result<(), AuthFailure> {
        let password = validate_new_password(password)?;
        let (token_id, secret) = parse_password_reset_token(token)?;
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT user_id, secret_hash FROM password_reset_tokens \
             WHERE token_id = $1 AND used_at IS NULL AND expires_at > $2 FOR UPDATE",
        )
        .bind(token_id)
        .bind(&now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_password_reset)?;
        let secret_hash: String = row.get("secret_hash");
        if !verify_password(secret, &secret_hash) {
            return Err(invalid_password_reset());
        }
        let user_id: String = row.get("user_id");
        let password_hash = hash_password(&password)?;
        sqlx::query("UPDATE users SET password_hash = $1, email_verified = TRUE WHERE id = $2")
            .bind(password_hash)
            .bind(&user_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 \
             WHERE user_id = $2 AND used_at IS NULL",
        )
        .bind(&now)
        .bind(&user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(&user_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    async fn create_session(
        &self,
        user_id: String,
        email: String,
        preferred_name: String,
    ) -> Result<CurrentSession, AuthFailure> {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let expires_at = now + Duration::days(SESSION_TTL_DAYS);
        sqlx::query("DELETE FROM sessions WHERE expires_at <= $1")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO sessions(token, user_id, created_at, expires_at) \
             VALUES($1, $2, $3, $4)",
        )
        .bind(&token)
        .bind(&user_id)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(CurrentSession {
            token,
            user_id,
            email,
            preferred_name,
        })
    }

    async fn session(&self, token: &str) -> Result<Option<CurrentSession>, AuthFailure> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query(
            "SELECT u.id, u.email, u.preferred_name FROM sessions s \
             JOIN users u ON u.id = s.user_id WHERE s.token = $1 AND s.expires_at > $2",
        )
        .bind(token)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(row.map(|row| CurrentSession {
            token: token.to_string(),
            user_id: row.get("id"),
            email: row.get("email"),
            preferred_name: row.get("preferred_name"),
        }))
    }

    async fn logout(&self, token: &str) -> Result<(), AuthFailure> {
        sqlx::query("DELETE FROM sessions WHERE token = $1")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(())
    }

    async fn update_profile(
        &self,
        user_id: &str,
        email: &str,
        preferred_name: &str,
    ) -> Result<CurrentSession, AuthFailure> {
        let email = normalize_email(email)?;
        let preferred_name = validate_preferred_name(preferred_name)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE users SET email = $1, preferred_name = $2, \
             email_verified = CASE WHEN lower(email) = lower($1) \
                 THEN email_verified ELSE FALSE END WHERE id = $3",
        )
        .bind(&email)
        .bind(&preferred_name)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("email_exists", "email is already registered")
            } else {
                AuthFailure::database(error)
            }
        })?;
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 \
             WHERE user_id = $2 AND used_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(CurrentSession {
            token: String::new(),
            user_id: user_id.to_string(),
            email,
            preferred_name,
        })
    }

    async fn admin_login(&self, password: &str) -> Result<AdminSession, AuthFailure> {
        if password != self.admin_password.as_ref() {
            return Err(AuthFailure::unauthorized(
                "invalid_admin_credentials",
                "invalid administrator password",
            ));
        }
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let expires_at = now + Duration::hours(ADMIN_SESSION_TTL_HOURS);
        sqlx::query("DELETE FROM admin_sessions WHERE expires_at <= $1")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query("INSERT INTO admin_sessions(token, created_at, expires_at) VALUES($1, $2, $3)")
            .bind(&token)
            .bind(now.to_rfc3339())
            .bind(expires_at.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(AdminSession { token })
    }

    async fn admin_session(&self, token: &str) -> Result<Option<AdminSession>, AuthFailure> {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM admin_sessions WHERE token = $1 AND expires_at > $2",
        )
        .bind(token)
        .bind(Utc::now().to_rfc3339())
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok((exists != 0).then(|| AdminSession {
            token: token.to_string(),
        }))
    }

    async fn admin_logout(&self, token: &str) -> Result<(), AuthFailure> {
        sqlx::query("DELETE FROM admin_sessions WHERE token = $1")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(())
    }

    async fn create_invitation(
        &self,
        organization_id: &str,
        created_by: &str,
    ) -> Result<(String, Url), AuthFailure> {
        self.require_manager(organization_id, created_by).await?;
        let token = format!("inv_{}", Uuid::new_v4().simple());
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO invitations(\
             token, created_at, created_by, kind, organization_id, role) \
             VALUES($1, $2, $3, 'organization', $4, 'member')",
        )
        .bind(&token)
        .bind(Utc::now().to_rfc3339())
        .bind(created_by)
        .bind(organization_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(created_by),
                source: "api",
                action: "invitation.created",
                resource_kind: "invitation",
                resource_id: organization_id,
                resource_name: None,
                payload: json!({ "role": "member" }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let mut url = self.app_public_url.clone();
        url.set_path("/");
        url.query_pairs_mut().clear().append_pair("invite", &token);
        Ok((token, url))
    }

    pub(crate) async fn create_personal_invitation(&self) -> Result<(String, Url), AuthFailure> {
        let token = format!("inv_{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO invitations(token, created_at, created_by, kind) \
             VALUES($1, $2, 'platform-admin', 'personal')",
        )
        .bind(&token)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let mut url = self.app_public_url.clone();
        url.set_path("/");
        url.query_pairs_mut().clear().append_pair("invite", &token);
        Ok((token, url))
    }

    pub async fn update_member_role(
        &self,
        organization_id: &str,
        actor: &str,
        target_user_id: &str,
        role: &str,
    ) -> Result<(), AuthFailure> {
        let actor_role = self
            .require_organization_member(organization_id, actor)
            .await?;
        if actor_role != "owner" {
            return Err(AuthFailure::forbidden(
                "organization_owner_required",
                "organization owner access required",
            ));
        }
        if !matches!(role, "admin" | "member") {
            return Err(AuthFailure::bad_request(
                "invalid_member_role",
                "member role must be admin or member",
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let target = sqlx::query(
            "SELECT m.role, u.preferred_name FROM organization_members m \
             JOIN users u ON u.id = m.user_id \
             WHERE m.organization_id = $1 AND m.user_id = $2 FOR UPDATE",
        )
        .bind(organization_id)
        .bind(target_user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let old_role = target.as_ref().map(|row| row.get::<String, _>("role"));
        let target_name = target
            .as_ref()
            .map(|row| row.get::<String, _>("preferred_name"));
        let result = sqlx::query(
            "UPDATE organization_members SET role = $1 \
             WHERE organization_id = $2 AND user_id = $3 AND role != 'owner'",
        )
        .bind(role)
        .bind(organization_id)
        .bind(target_user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "member_not_found",
                "member does not exist or is the organization owner",
            ));
        }
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(actor),
                source: "api",
                action: "member.role_updated",
                resource_kind: "organization_member",
                resource_id: target_user_id,
                resource_name: target_name.as_deref(),
                payload: json!({ "old_role": old_role, "new_role": role }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn remove_member(
        &self,
        organization_id: &str,
        actor: &str,
        target_user_id: &str,
    ) -> Result<(), AuthFailure> {
        self.require_manager(organization_id, actor).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let target = sqlx::query(
            "SELECT m.role, u.preferred_name FROM organization_members m \
             JOIN users u ON u.id = m.user_id \
             WHERE m.organization_id = $1 AND m.user_id = $2 FOR UPDATE",
        )
        .bind(organization_id)
        .bind(target_user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("member_not_found", "member does not exist"))?;
        let target_role: String = target.get("role");
        let target_name: String = target.get("preferred_name");
        if target_role == "owner" {
            return Err(AuthFailure::conflict(
                "owner_cannot_be_removed",
                "the organization owner cannot be removed",
            ));
        }
        sqlx::query(
            "DELETE FROM organization_members \
             WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
        .bind(target_user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(actor),
                source: "api",
                action: "member.removed",
                resource_kind: "organization_member",
                resource_id: target_user_id,
                resource_name: Some(&target_name),
                payload: json!({ "role": target_role }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn list_audit_events(
        &self,
        organization_id: &str,
        user_id: &str,
        workspace_id: Option<&str>,
        before: Option<i64>,
        limit: u16,
    ) -> Result<Vec<OrganizationAuditEvent>, AuthFailure> {
        self.require_manager(organization_id, user_id).await?;
        audit::list(
            &self.pool,
            organization_id,
            workspace_id,
            before,
            i64::from(limit.clamp(1, 100)),
        )
        .await
        .map_err(AuthFailure::database)
    }

    fn oauth_public_config(&self) -> Value {
        json!({
            "github": self.oauth.github.is_some(),
            "google": self.oauth.google.is_some(),
            "invitation_required": self.oauth.invitation_required,
        })
    }

    fn oauth_callback_url(&self, provider: &str) -> Url {
        let mut url = self.proxy_public_url.clone();
        url.set_path(&format!("/api/auth/oauth/{provider}/callback"));
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    async fn oauth_authorization_url(
        &self,
        provider: &str,
        invite: Option<&str>,
    ) -> Result<Url, AuthFailure> {
        let config = self
            .oauth
            .provider(provider)
            .ok_or_else(oauth_provider_unavailable)?;
        if invite.is_some_and(|value| value.is_empty() || value.len() > 256) {
            return Err(invalid_invitation());
        }
        let state = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM oauth_states WHERE expires_at <= $1")
            .bind(&now_text)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO oauth_states(state, provider, invite_token, created_at, expires_at) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&state)
        .bind(provider)
        .bind(invite)
        .bind(&now_text)
        .bind((now + Duration::minutes(OAUTH_STATE_TTL_MINUTES)).to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;

        let callback_url = self.oauth_callback_url(provider);
        let mut url = config.authorize_url.clone();
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", callback_url.as_str())
            .append_pair("response_type", "code")
            .append_pair("state", &state);
        match provider {
            "github" => {
                query.append_pair("scope", "user:email");
            }
            "google" => {
                query
                    .append_pair("scope", "openid email profile")
                    .append_pair("prompt", "select_account");
            }
            _ => return Err(oauth_provider_unavailable()),
        }
        drop(query);
        Ok(url)
    }

    async fn consume_oauth_state(
        &self,
        provider: &str,
        state: &str,
    ) -> Result<Option<String>, AuthFailure> {
        if state.len() != 64 || !state.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid_oauth_state());
        }
        sqlx::query_scalar::<_, Option<String>>(
            "DELETE FROM oauth_states WHERE state = $1 AND provider = $2 AND expires_at > $3 \
             RETURNING invite_token",
        )
        .bind(state)
        .bind(provider)
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_oauth_state)
    }

    async fn exchange_oauth_code(
        &self,
        provider: &str,
        code: &str,
    ) -> Result<OAuthProfile, AuthFailure> {
        if code.is_empty() || code.len() > 2048 {
            return Err(oauth_login_failed());
        }
        let config = self
            .oauth
            .provider(provider)
            .cloned()
            .ok_or_else(oauth_provider_unavailable)?;
        let callback_url = self.oauth_callback_url(provider);
        let response = self
            .oauth_client
            .post(config.token_url.clone())
            .header(header::ACCEPT, "application/json")
            .form(&[
                ("client_id", config.client_id.as_ref()),
                ("client_secret", config.client_secret.as_ref()),
                ("code", code),
                ("redirect_uri", callback_url.as_str()),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .map_err(oauth_request_failed)?;
        let status = response.status();
        let token = response
            .json::<OAuthTokenResponse>()
            .await
            .map_err(oauth_request_failed)?;
        let Some(access_token) = token.access_token else {
            tracing::warn!(
                provider,
                %status,
                error = token.error.as_deref().unwrap_or("unknown"),
                description = token.error_description.as_deref().unwrap_or(""),
                "OAuth token exchange failed"
            );
            return Err(oauth_login_failed());
        };
        if !status.is_success() {
            tracing::warn!(provider, %status, "OAuth token endpoint returned an error");
            return Err(oauth_login_failed());
        }
        match provider {
            "github" => self.github_profile(&config, &access_token).await,
            "google" => self.google_profile(&config, &access_token).await,
            _ => Err(oauth_provider_unavailable()),
        }
    }

    async fn github_profile(
        &self,
        config: &OAuthProviderConfig,
        access_token: &str,
    ) -> Result<OAuthProfile, AuthFailure> {
        let user_response = self
            .oauth_client
            .get(config.user_url.clone())
            .bearer_auth(access_token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(oauth_request_failed)?;
        if !user_response.status().is_success() {
            tracing::warn!(status = %user_response.status(), "GitHub user API failed");
            return Err(oauth_login_failed());
        }
        let user = user_response
            .json::<GithubUser>()
            .await
            .map_err(oauth_request_failed)?;
        let emails_url = config
            .emails_url
            .clone()
            .ok_or_else(oauth_provider_unavailable)?;
        let emails_response = self
            .oauth_client
            .get(emails_url)
            .bearer_auth(access_token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(oauth_request_failed)?;
        if !emails_response.status().is_success() {
            tracing::warn!(status = %emails_response.status(), "GitHub email API failed");
            return Err(verified_email_required());
        }
        let emails = emails_response
            .json::<Vec<GithubEmail>>()
            .await
            .map_err(oauth_request_failed)?;
        let email = emails
            .iter()
            .find(|email| email.primary && email.verified)
            .or_else(|| emails.iter().find(|email| email.verified))
            .ok_or_else(verified_email_required)?;
        let email = normalize_email(&email.email)?;
        let preferred_name = provider_preferred_name(user.name.as_deref(), &user.login, &email)?;
        Ok(OAuthProfile {
            provider: "github",
            subject: user.id.to_string(),
            email,
            preferred_name,
        })
    }

    async fn google_profile(
        &self,
        config: &OAuthProviderConfig,
        access_token: &str,
    ) -> Result<OAuthProfile, AuthFailure> {
        let response = self
            .oauth_client
            .get(config.user_url.clone())
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(oauth_request_failed)?;
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), "Google UserInfo API failed");
            return Err(oauth_login_failed());
        }
        let user = response
            .json::<GoogleUser>()
            .await
            .map_err(oauth_request_failed)?;
        if !user.email_verified {
            return Err(verified_email_required());
        }
        let email = normalize_email(&user.email)?;
        let fallback = email.split('@').next().unwrap_or("Treer user");
        let preferred_name = provider_preferred_name(user.name.as_deref(), fallback, &email)?;
        if user.sub.is_empty() || user.sub.len() > 255 {
            return Err(oauth_login_failed());
        }
        Ok(OAuthProfile {
            provider: "google",
            subject: user.sub,
            email,
            preferred_name,
        })
    }

    async fn complete_oauth_login(
        &self,
        profile: OAuthProfile,
        invite: Option<&str>,
    ) -> Result<CurrentSession, AuthFailure> {
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let linked = sqlx::query(
            "SELECT u.id, u.email, u.preferred_name FROM oauth_identities i \
             JOIN users u ON u.id = i.user_id \
             WHERE i.provider = $1 AND i.subject = $2 FOR UPDATE",
        )
        .bind(profile.provider)
        .bind(&profile.subject)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if let Some(user) = linked {
            sqlx::query(
                "UPDATE oauth_identities SET email = $1, updated_at = $2 \
                 WHERE provider = $3 AND subject = $4",
            )
            .bind(&profile.email)
            .bind(&now)
            .bind(profile.provider)
            .bind(&profile.subject)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            let user_id = user.get("id");
            let email = user.get("email");
            let preferred_name = user.get("preferred_name");
            transaction.commit().await.map_err(AuthFailure::database)?;
            return self.create_session(user_id, email, preferred_name).await;
        }

        let existing_user = sqlx::query(
            "SELECT id, email, preferred_name, email_verified FROM users \
             WHERE lower(email) = lower($1) FOR UPDATE",
        )
        .bind(&profile.email)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let (user_id, email, preferred_name, created) = if let Some(user) = existing_user {
            if !user.get::<bool, _>("email_verified") {
                let password_hash = hash_password(&format!(
                    "{}{}",
                    Uuid::new_v4().simple(),
                    Uuid::new_v4().simple()
                ))?;
                let user_id: String = user.get("id");
                sqlx::query(
                    "UPDATE users SET password_hash = $1, email_verified = TRUE WHERE id = $2",
                )
                .bind(password_hash)
                .bind(&user_id)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
                sqlx::query("DELETE FROM sessions WHERE user_id = $1")
                    .bind(&user_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(AuthFailure::database)?;
            }
            (
                user.get("id"),
                user.get("email"),
                user.get("preferred_name"),
                false,
            )
        } else {
            let invitation = self
                .load_registration_invitation(&mut transaction, invite)
                .await?;
            let password_hash = hash_password(&format!(
                "{}{}",
                Uuid::new_v4().simple(),
                Uuid::new_v4().simple()
            ))?;
            let user_id = insert_user(
                &mut transaction,
                &profile.email,
                &profile.preferred_name,
                password_hash,
                true,
                &now,
            )
            .await?;
            apply_registration_membership(
                &mut transaction,
                invitation,
                &user_id,
                &profile.preferred_name,
                &now,
            )
            .await?;
            (
                user_id,
                profile.email.clone(),
                profile.preferred_name.clone(),
                true,
            )
        };
        sqlx::query(
            "INSERT INTO oauth_identities(provider, subject, user_id, email, created_at, updated_at) \
             VALUES($1, $2, $3, $4, $5, $5)",
        )
        .bind(profile.provider)
        .bind(&profile.subject)
        .bind(&user_id)
        .bind(&profile.email)
        .bind(&now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict(
                    "oauth_identity_conflict",
                    "this OAuth identity is already linked",
                )
            } else {
                AuthFailure::database(error)
            }
        })?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let session = self.create_session(user_id, email, preferred_name).await?;
        if created {
            self.send_welcome_email(&session);
        }
        Ok(session)
    }

    async fn register(
        &self,
        invite: Option<&str>,
        email: &str,
        preferred_name: &str,
        password: &str,
    ) -> Result<CurrentSession, AuthFailure> {
        let email = normalize_email(email)?;
        let preferred_name = validate_preferred_name(preferred_name)?;
        let password = validate_new_password(password)?;
        let password_hash = hash_password(&password)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let invitation = self
            .load_registration_invitation(&mut transaction, invite)
            .await?;
        let now = Utc::now().to_rfc3339();
        let user_id = insert_user(
            &mut transaction,
            &email,
            &preferred_name,
            password_hash,
            false,
            &now,
        )
        .await?;
        apply_registration_membership(
            &mut transaction,
            invitation,
            &user_id,
            &preferred_name,
            &now,
        )
        .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let session = self.create_session(user_id, email, preferred_name).await?;
        self.send_welcome_email(&session);
        Ok(session)
    }

    async fn load_registration_invitation(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        invite: Option<&str>,
    ) -> Result<Option<RegistrationInvitation>, AuthFailure> {
        let Some(invite) = invite.filter(|value| !value.is_empty()) else {
            if self.oauth.invitation_required {
                return Err(AuthFailure::bad_request(
                    "invitation_required",
                    "a valid invitation is required to create an account",
                ));
            }
            return Ok(None);
        };
        let invitation = sqlx::query(
            "SELECT kind, organization_id, role FROM invitations \
             WHERE token = $1 AND used_at IS NULL FOR UPDATE",
        )
        .bind(invite)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_invitation)?;
        Ok(Some(RegistrationInvitation {
            token: invite.to_string(),
            kind: invitation.get("kind"),
            organization_id: invitation.get("organization_id"),
            role: invitation.get("role"),
        }))
    }

    fn send_welcome_email(&self, session: &CurrentSession) {
        let Some(sender) = self.email_sender.clone() else {
            return;
        };
        let recipient = session.email.clone();
        let preferred_name = session.preferred_name.clone();
        let app_url = self.app_public_url.clone();
        tokio::spawn(async move {
            if let Err(error) = sender
                .send_welcome(&recipient, &preferred_name, &app_url)
                .await
            {
                tracing::error!(%error, "failed to send registration welcome email");
            }
        });
    }
}

async fn insert_user(
    transaction: &mut Transaction<'_, Postgres>,
    email: &str,
    preferred_name: &str,
    password_hash: String,
    email_verified: bool,
    now: &str,
) -> Result<String, AuthFailure> {
    let user_id = format!("usr_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO users(id, email, preferred_name, password_hash, email_verified, created_at) \
         VALUES($1, $2, $3, $4, $5, $6)",
    )
    .bind(&user_id)
    .bind(email)
    .bind(preferred_name)
    .bind(password_hash)
    .bind(email_verified)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(|error| error.is_unique_violation())
        {
            AuthFailure::conflict("email_exists", "email is already registered")
        } else {
            AuthFailure::database(error)
        }
    })?;
    Ok(user_id)
}

async fn apply_registration_membership(
    transaction: &mut Transaction<'_, Postgres>,
    invitation: Option<RegistrationInvitation>,
    user_id: &str,
    preferred_name: &str,
    now: &str,
) -> Result<(), AuthFailure> {
    if let Some(invitation) = &invitation {
        let result = sqlx::query(
            "UPDATE invitations SET used_at = $1, used_by = $2 \
             WHERE token = $3 AND used_at IS NULL",
        )
        .bind(now)
        .bind(user_id)
        .bind(&invitation.token)
        .execute(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(invalid_invitation());
        }
    }

    match invitation.as_ref().map(|value| value.kind.as_str()) {
        None | Some("personal") => {
            let organization_id = format!("org_{}", Uuid::new_v4().simple());
            let organization_name = format!("{preferred_name} Personal");
            sqlx::query(
                "INSERT INTO organizations(organization_id, name, created_at, created_by) \
                 VALUES($1, $2, $3, $4)",
            )
            .bind(&organization_id)
            .bind(organization_name)
            .bind(now)
            .bind(user_id)
            .execute(&mut **transaction)
            .await
            .map_err(AuthFailure::database)?;
            sqlx::query(
                "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
                 VALUES($1, $2, 'owner', $3)",
            )
            .bind(organization_id)
            .bind(user_id)
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        Some("organization") => {
            let invitation = invitation.as_ref().expect("matched invitation");
            let organization_id = invitation.organization_id.as_deref().ok_or_else(|| {
                AuthFailure::internal(
                    "invalid_invitation_state",
                    "organization invitation has no organization".to_string(),
                )
            })?;
            let role = invitation.role.as_deref().ok_or_else(|| {
                AuthFailure::internal(
                    "invalid_invitation_state",
                    "organization invitation has no role".to_string(),
                )
            })?;
            sqlx::query(
                "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
                 VALUES($1, $2, $3, $4)",
            )
            .bind(organization_id)
            .bind(user_id)
            .bind(role)
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        Some(_) => {
            return Err(AuthFailure::internal(
                "invalid_invitation_state",
                "invitation has an unsupported kind".to_string(),
            ));
        }
    }
    Ok(())
}

pub async fn require_user(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    match authenticate_request(&auth, request.headers()).await {
        Ok(session) => {
            request.extensions_mut().insert(session);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

pub async fn require_workspace_access(
    State(auth): State<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    request: Request,
    next: Next,
) -> Response {
    let Some(workspace_id) = workspace_id_from_api_path(request.uri().path()) else {
        return next.run(request).await;
    };
    match auth
        .require_workspace_member(&workspace_id, &session.user_id)
        .await
    {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

pub async fn require_admin(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    if auth.disabled {
        request.extensions_mut().insert(AdminSession {
            token: "local-admin".to_string(),
        });
        return next.run(request).await;
    }
    let result = cookie_value(request.headers(), ADMIN_SESSION_COOKIE).ok_or_else(|| {
        AuthFailure::unauthorized(
            "admin_authentication_required",
            "administrator authentication required",
        )
    });
    match result {
        Ok(token) => match auth.admin_session(&token).await {
            Ok(Some(session)) => {
                request.extensions_mut().insert(session);
                next.run(request).await
            }
            Ok(None) => AuthFailure::unauthorized(
                "admin_authentication_required",
                "administrator authentication required",
            )
            .into_response(),
            Err(error) => error.into_response(),
        },
        Err(error) => error.into_response(),
    }
}

pub async fn require_machine(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    match auth.authenticate_machine(request.headers()).await {
        Ok(session) if machine_workspace_matches(&session, request.uri().path()) => {
            match auth.authenticate_agent(&session, request.headers()).await {
                Ok(agent) => {
                    request.extensions_mut().insert(session);
                    if let Some(agent) = agent {
                        request.extensions_mut().insert(agent);
                    }
                    next.run(request).await
                }
                Err(error) => error.into_response(),
            }
        }
        Ok(_) => AuthFailure::forbidden(
            "machine_workspace_mismatch",
            "machine credentials do not grant access to this workspace",
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn authenticate_request(
    auth: &AuthStore,
    headers: &HeaderMap,
) -> Result<CurrentSession, AuthFailure> {
    if auth.disabled {
        return Ok(CurrentSession {
            token: "local".to_string(),
            user_id: "local".to_string(),
            email: "local@treer.invalid".to_string(),
            preferred_name: "Local user".to_string(),
        });
    }
    let token = cookie_value(headers, SESSION_COOKIE).ok_or_else(|| {
        AuthFailure::unauthorized("authentication_required", "authentication required")
    })?;
    auth.session(&token).await?.ok_or_else(|| {
        AuthFailure::unauthorized("authentication_required", "authentication required")
    })
}

pub async fn login(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<LoginRequest>,
) -> Result<Response, AuthFailure> {
    let session = auth.login(&request.email, &request.password).await?;
    Ok(session_response(&auth, &session))
}

pub async fn oauth_config(Extension(auth): Extension<AuthStore>) -> Json<Value> {
    Json(auth.oauth_public_config())
}

pub async fn oauth_start(
    Extension(auth): Extension<AuthStore>,
    AxumPath(provider): AxumPath<String>,
    Query(query): Query<OAuthStartQuery>,
) -> Result<Redirect, AuthFailure> {
    let url = auth
        .oauth_authorization_url(&provider, query.invite.as_deref())
        .await?;
    Ok(Redirect::temporary(url.as_str()))
}

pub async fn oauth_callback(
    Extension(auth): Extension<AuthStore>,
    AxumPath(provider): AxumPath<String>,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    let result = async {
        let state = query.state.as_deref().ok_or_else(invalid_oauth_state)?;
        let invite = auth.consume_oauth_state(&provider, state).await?;
        if query.error.is_some() {
            return Err(oauth_login_failed());
        }
        let code = query.code.as_deref().ok_or_else(oauth_login_failed)?;
        let profile = auth.exchange_oauth_code(&provider, code).await?;
        auth.complete_oauth_login(profile, invite.as_deref()).await
    }
    .await;
    match result {
        Ok(session) => oauth_session_redirect(&auth, &session),
        Err(error) => {
            tracing::warn!(?error, provider, "OAuth callback failed");
            oauth_error_redirect(&auth)
        }
    }
}

pub async fn request_password_reset(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<RequestPasswordResetRequest>,
) -> Result<Json<Value>, AuthFailure> {
    auth.request_password_reset(&request.email).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn reset_password(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<ResetPasswordRequest>,
) -> Result<Json<Value>, AuthFailure> {
    auth.reset_password(&request.token, &request.password)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn register(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<RegisterRequest>,
) -> Result<Response, AuthFailure> {
    let session = auth
        .register(
            request.invite.as_deref(),
            &request.email,
            &request.preferred_name,
            &request.password,
        )
        .await?;
    Ok(session_response(&auth, &session))
}

pub async fn me(Extension(session): Extension<CurrentSession>) -> Json<Value> {
    Json(user_json(&session))
}

pub async fn update_profile(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Json(request): Json<UpdateProfileRequest>,
) -> Result<Json<Value>, AuthFailure> {
    let user = auth
        .update_profile(&session.user_id, &request.email, &request.preferred_name)
        .await?;
    Ok(Json(user_json(&user)))
}

pub async fn organizations(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "organizations": auth.list_organizations(&session.user_id).await?
    })))
}

pub async fn create_organization_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Json(request): Json<CreateOrganizationRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "organization": auth.create_organization(&session.user_id, &request.name).await?
    })))
}

pub async fn rename_organization_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
    Json(request): Json<RenameOrganizationRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "organization": auth
            .rename_organization(&organization_id, &session.user_id, &request.name)
            .await?
    })))
}

pub async fn members(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<Json<Value>, AuthFailure> {
    let role = auth
        .require_organization_member(&organization_id, &session.user_id)
        .await?;
    Ok(Json(json!({
        "members": auth.list_members(&organization_id, &session.user_id).await?,
        "current_role": role
    })))
}

pub async fn audit_events(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
    Query(query): Query<AuditEventsQuery>,
) -> Result<Json<Value>, AuthFailure> {
    let events = auth
        .list_audit_events(
            &organization_id,
            &session.user_id,
            query.workspace_id.as_deref(),
            query.before,
            query.limit,
        )
        .await?;
    let next_cursor = events.last().map(|event| event.sequence);
    Ok(Json(
        json!({ "events": events, "next_cursor": next_cursor }),
    ))
}

pub async fn logout(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
) -> Result<Response, AuthFailure> {
    auth.logout(&session.token).await?;
    let cookie = format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        secure_cookie_suffix(&auth)
    );
    Ok((
        [(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).map_err(AuthFailure::header)?,
        )],
        Json(json!({ "ok": true })),
    )
        .into_response())
}

pub async fn create_invitation(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<Json<Value>, AuthFailure> {
    let (token, url) = auth
        .create_invitation(&organization_id, &session.user_id)
        .await?;
    Ok(Json(json!({ "token": token, "url": url.as_str() })))
}

pub async fn update_member_role_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, user_id)): AxumPath<(String, String)>,
    Json(request): Json<UpdateMemberRoleRequest>,
) -> Result<Json<Value>, AuthFailure> {
    auth.update_member_role(&organization_id, &session.user_id, &user_id, &request.role)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove_member_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, user_id)): AxumPath<(String, String)>,
) -> Result<Json<Value>, AuthFailure> {
    auth.remove_member(&organization_id, &session.user_id, &user_id)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn admin_login(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<AdminLoginRequest>,
) -> Result<Response, AuthFailure> {
    let session = auth.admin_login(&request.password).await?;
    let cookie = format!(
        "{ADMIN_SESSION_COOKIE}={}; Path=/api/admin; HttpOnly; SameSite=Strict; Max-Age={}{}",
        session.token,
        ADMIN_SESSION_TTL_HOURS * 60 * 60,
        secure_cookie_suffix(&auth)
    );
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "admin": true })),
    )
        .into_response())
}

pub async fn admin_me() -> Json<Value> {
    Json(json!({ "admin": true }))
}

pub async fn admin_logout(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<AdminSession>,
) -> Result<Response, AuthFailure> {
    if !auth.disabled {
        auth.admin_logout(&session.token).await?;
    }
    let cookie = format!(
        "{ADMIN_SESSION_COOKIE}=; Path=/api/admin; HttpOnly; SameSite=Strict; Max-Age=0{}",
        secure_cookie_suffix(&auth)
    );
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

fn session_response(auth: &AuthStore, session: &CurrentSession) -> Response {
    let cookie = format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        session.token,
        SESSION_TTL_DAYS * 24 * 60 * 60,
        secure_cookie_suffix(auth)
    );
    ([(header::SET_COOKIE, cookie)], Json(user_json(session))).into_response()
}

fn oauth_session_redirect(auth: &AuthStore, session: &CurrentSession) -> Response {
    let cookie = format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        session.token,
        SESSION_TTL_DAYS * 24 * 60 * 60,
        secure_cookie_suffix(auth)
    );
    let mut response = Redirect::to(auth.app_public_url.as_str()).into_response();
    match HeaderValue::from_str(&cookie) {
        Ok(cookie) => {
            response.headers_mut().insert(header::SET_COOKIE, cookie);
            response
        }
        Err(error) => AuthFailure::header(error).into_response(),
    }
}

fn oauth_error_redirect(auth: &AuthStore) -> Response {
    let mut url = auth.app_public_url.clone();
    url.query_pairs_mut()
        .append_pair("oauth_error", "login_failed");
    Redirect::to(url.as_str()).into_response()
}

fn user_json(session: &CurrentSession) -> Value {
    json!({
        "user_id": session.user_id,
        "email": session.email,
        "preferred_name": session.preferred_name,
    })
}

fn secure_cookie_suffix(auth: &AuthStore) -> &'static str {
    if auth.secure_cookies {
        "; Secure"
    } else {
        ""
    }
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

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
}

fn optional_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, AuthFailure> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .ok()
                .filter(|value| !value.is_empty())
                .ok_or_else(agent_auth_required)
        })
        .transpose()
}

fn agent_auth_required() -> AuthFailure {
    AuthFailure::unauthorized(
        "agent_authentication_required",
        "valid Agent workload credentials are required",
    )
}

fn fast_secret_hash(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

fn valid_pkce_verifier(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

fn valid_pkce_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn fast_secret_matches(secret: &str, expected_hash: &str) -> bool {
    let actual = fast_secret_hash(secret);
    actual.len() == expected_hash.len()
        && actual
            .as_bytes()
            .ct_eq(expected_hash.as_bytes())
            .unwrap_u8()
            == 1
}

fn machine_workspace_matches(session: &MachineSession, path: &str) -> bool {
    if path == "/agent/machine/identity" {
        return session.server_id.is_some() && session.workspace_id.is_some();
    }
    let Some(encoded_workspace) = path
        .strip_prefix("/agent/workspaces/")
        .and_then(|rest| rest.split('/').next())
    else {
        return false;
    };
    percent_encoding::percent_decode_str(encoded_workspace)
        .decode_utf8()
        .is_ok_and(|workspace| session.allows_workspace(&workspace))
}

fn workspace_id_from_api_path(path: &str) -> Option<String> {
    let encoded = path.strip_prefix("/api/workspaces/")?.split('/').next()?;
    percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()
        .map(|workspace| workspace.into_owned())
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn parse_credential<'a>(
    token: &'a str,
    expected_prefix: &str,
) -> Result<(&'a str, &'a str), AuthFailure> {
    let (identifier, secret) = token.split_once('.').ok_or_else(machine_auth_required)?;
    if !identifier.starts_with(expected_prefix) || secret.len() != 64 {
        return Err(machine_auth_required());
    }
    Ok((identifier, secret))
}

fn machine_auth_required() -> AuthFailure {
    AuthFailure::unauthorized(
        "machine_authentication_required",
        "valid machine credentials are required",
    )
}

fn invalid_machine_enrollment() -> AuthFailure {
    AuthFailure::unauthorized(
        "invalid_machine_enrollment",
        "machine enrollment token is invalid, expired, or already used",
    )
}

fn invalid_password_reset() -> AuthFailure {
    AuthFailure::bad_request(
        "invalid_password_reset",
        "password reset link is invalid or expired",
    )
}

fn invalid_invitation() -> AuthFailure {
    AuthFailure::bad_request(
        "invalid_invitation",
        "invitation is invalid or already used",
    )
}

fn oauth_provider_unavailable() -> AuthFailure {
    AuthFailure::not_found(
        "oauth_provider_unavailable",
        "this OAuth provider is not configured",
    )
}

fn invalid_oauth_state() -> AuthFailure {
    AuthFailure::bad_request(
        "invalid_oauth_state",
        "the OAuth login request is invalid or expired",
    )
}

fn oauth_login_failed() -> AuthFailure {
    AuthFailure::unauthorized("oauth_login_failed", "OAuth login failed")
}

fn verified_email_required() -> AuthFailure {
    AuthFailure::forbidden(
        "verified_email_required",
        "the OAuth provider must provide a verified email address",
    )
}

fn oauth_request_failed(error: reqwest::Error) -> AuthFailure {
    tracing::warn!(%error, "OAuth provider request failed");
    oauth_login_failed()
}

fn provider_preferred_name(
    candidate: Option<&str>,
    fallback: &str,
    email: &str,
) -> Result<String, AuthFailure> {
    if let Some(candidate) = candidate {
        if let Ok(name) = validate_preferred_name(candidate) {
            return Ok(name);
        }
    }
    if let Ok(name) = validate_preferred_name(fallback) {
        return Ok(name);
    }
    validate_preferred_name(email.split('@').next().unwrap_or("Treer user"))
}

fn invalid_ingress_authorization() -> AuthFailure {
    AuthFailure::unauthorized(
        "invalid_ingress_authorization",
        "ingress authorization is invalid or expired",
    )
}

fn invalid_app_oauth_code() -> AuthFailure {
    AuthFailure::unauthorized(
        "invalid_app_oauth_code",
        "app OAuth code is invalid, expired, or already used",
    )
}

fn parse_password_reset_token(token: &str) -> Result<(&str, &str), AuthFailure> {
    let (token_id, secret) = token.split_once('.').ok_or_else(invalid_password_reset)?;
    if token_id.len() != 36
        || !token_id.starts_with("pwd_")
        || !token_id[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
        || secret.len() != 64
        || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_password_reset());
    }
    Ok((token_id, secret))
}

fn validate_new_password(password: &str) -> Result<String, AuthFailure> {
    if password.len() < 8 {
        return Err(AuthFailure::bad_request(
            "invalid_password",
            "password must contain at least 8 characters",
        ));
    }
    if password.len() > 1024 {
        return Err(AuthFailure::bad_request(
            "invalid_password",
            "password must contain at most 1024 characters",
        ));
    }
    Ok(password.to_string())
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn normalize_email(email: &str) -> Result<String, AuthFailure> {
    let email = email.trim().to_ascii_lowercase();
    let valid = email.len() <= 254
        && !email
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && local.len() <= 64
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if !valid {
        return Err(AuthFailure::bad_request(
            "invalid_email",
            "enter a valid email address",
        ));
    }
    Ok(email)
}

fn validate_preferred_name(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return Err(AuthFailure::bad_request(
            "invalid_preferred_name",
            "preferred name must contain 1-80 visible characters",
        ));
    }
    Ok(value.to_string())
}

fn validate_resource_name(value: &str, resource: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty() || value.len() > 80 || value.chars().any(|character| character.is_control())
    {
        return Err(AuthFailure::bad_request(
            "invalid_name",
            &format!("{resource} name must be 1-80 printable characters"),
        ));
    }
    Ok(value.to_string())
}

fn validate_installation_id(value: &str) -> Result<String, AuthFailure> {
    if value.len() != 36
        || !value.starts_with("mid_")
        || !value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AuthFailure::bad_request(
            "invalid_machine_identity",
            "machine installation identity is invalid",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn validate_machine_server_id(value: &str) -> Result<String, AuthFailure> {
    if value.len() != 36
        || !value.starts_with("srv_")
        || !value[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AuthFailure::bad_request(
            "invalid_machine_identity",
            "installed machine ID is invalid",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn workspace_from_row(row: sqlx::postgres::PgRow) -> Result<WorkspaceInfo, AuthFailure> {
    let created_at: String = row.get("created_at");
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            tracing::error!(%error, "invalid workspace timestamp in database");
            AuthFailure::internal(
                "database_error",
                "workspace timestamp is invalid".to_string(),
            )
        })?;
    Ok(WorkspaceInfo {
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        created_at,
    })
}

fn agent_launch_profile_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<AgentLaunchProfile, AuthFailure> {
    let args = serde_json::from_value::<Vec<String>>(row.get("args")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("agent launch profile has invalid args: {error}"),
        )
    })?;
    Ok(AgentLaunchProfile {
        profile_id: row.get("profile_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        description: row.get("description"),
        cwd: row.get("cwd"),
        command: row.get("command"),
        args,
        created_at: parse_database_timestamp(&row, "created_at", "agent launch profile")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "agent launch profile")?,
        updated_by: row.get("updated_by"),
    })
}

fn app_deployment_from_row(row: sqlx::postgres::PgRow) -> Result<AppDeployment, AuthFailure> {
    let args = serde_json::from_value::<Vec<String>>(row.get("args")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("App deployment has invalid args: {error}"),
        )
    })?;
    let port = u16::try_from(row.get::<i64, _>("port")).map_err(|_| {
        AuthFailure::internal(
            "database_error",
            "App deployment has invalid port".to_string(),
        )
    })?;
    let restart_count = u64::try_from(row.get::<i64, _>("restart_count")).map_err(|_| {
        AuthFailure::internal(
            "database_error",
            "App deployment has invalid restart count".to_string(),
        )
    })?;
    let desired_state = match row.get::<String, _>("desired_state").as_str() {
        "running" => AppDesiredState::Running,
        "stopped" => AppDesiredState::Stopped,
        value => {
            return Err(AuthFailure::internal(
                "database_error",
                format!("App deployment has invalid desired state {value}"),
            ))
        }
    };
    Ok(AppDeployment {
        app_id: row.get("app_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        server_id: row.get("server_id"),
        command: row.get("command"),
        args,
        cwd: row.get("cwd"),
        port,
        hostname: row.get("hostname"),
        service_id: row.get("service_id"),
        public_url: None,
        desired_state,
        runtime_agent_id: row.get("runtime_agent_id"),
        restart_count,
        status: AppDeploymentStatus::Pending,
        pid: None,
        exit_code: None,
        last_error: row.get("last_error"),
        created_at: parse_database_timestamp(&row, "created_at", "App deployment")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "App deployment")?,
        updated_by: row.get("updated_by"),
    })
}

const fn app_desired_state_str(state: AppDesiredState) -> &'static str {
    match state {
        AppDesiredState::Running => "running",
        AppDesiredState::Stopped => "stopped",
    }
}

fn app_deployment_write_error(error: sqlx::Error) -> AuthFailure {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        AuthFailure::conflict(
            "app_conflict",
            "App name, service name, or virtual hostname already exists",
        )
    } else {
        AuthFailure::database(error)
    }
}

fn validate_launch_profile_description(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.chars().count() > MAX_LAUNCH_PROFILE_DESCRIPTION_CHARS
        || value.chars().any(|character| character == '\0')
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile description must be at most 1000 characters and contain no NUL bytes",
        ));
    }
    Ok(value.to_string())
}

fn validate_launch_profile_cwd(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    let value = if value.is_empty() { "." } else { value };
    if value.len() > MAX_LAUNCH_PROFILE_CWD_BYTES || value.contains('\0') {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile working directory is too long or contains a NUL byte",
        ));
    }
    Ok(value.to_string())
}

fn validate_launch_profile_command(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_LAUNCH_PROFILE_COMMAND_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile command must be 1-4096 printable characters",
        ));
    }
    Ok(value.to_string())
}

fn validate_launch_profile_args(args: Vec<String>) -> Result<Vec<String>, AuthFailure> {
    let total_bytes = args.iter().map(String::len).sum::<usize>();
    if args.len() > MAX_LAUNCH_PROFILE_ARGS
        || total_bytes > MAX_LAUNCH_PROFILE_ARGS_BYTES
        || args
            .iter()
            .any(|arg| arg.len() > MAX_LAUNCH_PROFILE_ARG_BYTES || arg.contains('\0'))
    {
        return Err(AuthFailure::bad_request(
            "invalid_launch_profile",
            "launch profile args exceed the count or size limit or contain a NUL byte",
        ));
    }
    Ok(args)
}

fn launch_profile_write_error(error: sqlx::Error) -> AuthFailure {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        AuthFailure::conflict(
            "launch_profile_exists",
            "a launch profile with this name already exists",
        )
    } else {
        AuthFailure::database(error)
    }
}

async fn insert_default_agent_launch_profiles(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: &str,
    user_id: &str,
    now: &chrono::DateTime<Utc>,
) -> Result<(), AuthFailure> {
    let timestamp = now.to_rfc3339();
    for (name, description, command) in DEFAULT_AGENT_LAUNCH_PROFILES {
        sqlx::query(
            "INSERT INTO agent_launch_profiles(\
             profile_id, workspace_id, name, description, cwd, command, args, created_at, \
             created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, '.', $5, $6, $7, $8, $7, $8)",
        )
        .bind(format!("alp_{}", Uuid::new_v4().simple()))
        .bind(workspace_id)
        .bind(name)
        .bind(description)
        .bind(command)
        .bind(json!([]))
        .bind(&timestamp)
        .bind(user_id)
        .execute(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
    }
    Ok(())
}

async fn insert_launch_profile_audit(
    transaction: &mut Transaction<'_, Postgres>,
    profile: &AgentLaunchProfile,
    actor: ProfileMutationActor<'_>,
    action: &str,
) -> Result<(), AuthFailure> {
    let organization_id = sqlx::query_scalar::<_, String>(
        "SELECT organization_id FROM workspaces WHERE workspace_id = $1",
    )
    .bind(&profile.workspace_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(AuthFailure::database)?;
    audit::insert(
        transaction,
        NewAuditEvent {
            organization_id: &organization_id,
            workspace_id: Some(&profile.workspace_id),
            actor_kind: actor.kind,
            actor_id: actor.id,
            source: "api",
            action,
            resource_kind: "agent_launch_profile",
            resource_id: &profile.profile_id,
            resource_name: Some(&profile.name),
            payload: json!({}),
        },
    )
    .await
    .map_err(AuthFailure::database)
}

fn machine_service_from_row(row: sqlx::postgres::PgRow) -> Result<MachineService, AuthFailure> {
    let target_port = u16::try_from(row.get::<i64, _>("target_port")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("machine service has invalid target_port: {error}"),
        )
    })?;
    if target_port == 0 {
        return Err(AuthFailure::internal(
            "database_error",
            "machine service target_port is zero".to_string(),
        ));
    }
    let protocol = match row.get::<String, _>("protocol").as_str() {
        "tcp" => MachineServiceProtocol::Tcp,
        "http" => MachineServiceProtocol::Http,
        value => {
            return Err(AuthFailure::internal(
                "database_error",
                format!("machine service has invalid protocol {value}"),
            ))
        }
    };
    Ok(MachineService {
        service_id: row.get("service_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        server_id: row.get("server_id"),
        target_agent_id: row.get("target_agent_id"),
        target_host: row.get("target_host"),
        target_port,
        protocol,
        created_at: parse_database_timestamp(&row, "created_at", "machine service")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "machine service")?,
        updated_by: row.get("updated_by"),
    })
}

fn parse_database_timestamp(
    row: &sqlx::postgres::PgRow,
    column: &str,
    resource: &str,
) -> Result<chrono::DateTime<Utc>, AuthFailure> {
    row.get::<String, _>(column).parse().map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("{resource} has invalid {column}: {error}"),
        )
    })
}

fn validate_service_target_host(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 253
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(AuthFailure::bad_request(
            "invalid_service",
            "target_host must be a non-empty hostname or address",
        ));
    }
    Ok(value.to_string())
}

fn validate_service_target_agent_id(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 255
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(AuthFailure::bad_request(
            "invalid_service",
            "target_agent_id must be a non-empty Agent identifier",
        ));
    }
    Ok(value.to_string())
}

fn validate_agent_service_target_host(value: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if !matches!(value, "127.0.0.1" | "localhost" | "::1") {
        return Err(AuthFailure::bad_request(
            "invalid_agent_service_target",
            "Agent services may only target the Agent loopback interface",
        ));
    }
    Ok("127.0.0.1".to_string())
}

const fn machine_service_protocol_str(protocol: MachineServiceProtocol) -> &'static str {
    match protocol {
        MachineServiceProtocol::Tcp => "tcp",
        MachineServiceProtocol::Http => "http",
    }
}

const fn service_ingress_access_str(access: ServiceIngressAccess) -> &'static str {
    match access {
        ServiceIngressAccess::Public => "public",
        ServiceIngressAccess::Workspace => "workspace",
    }
}

fn normalize_ingress_slug(value: &str) -> Result<String, AuthFailure> {
    let mut slug = String::new();
    let mut separator = false;
    for byte in value.trim().bytes() {
        if byte.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() && slug.len() < 40 {
                slug.push('-');
            }
            if slug.len() < 40 {
                slug.push(byte.to_ascii_lowercase() as char);
            }
            separator = false;
        } else {
            separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_slug",
            "ingress slug must contain an ASCII letter or number",
        ));
    }
    Ok(slug)
}

pub(crate) fn managed_app_ingress_hostname(
    app_name: &str,
    app_id: &str,
    base_domain: &str,
) -> Result<String, AuthFailure> {
    let slug = normalize_ingress_slug(app_name).unwrap_or_else(|_| "app".to_string());
    let suffix = app_id
        .strip_prefix("app_")
        .unwrap_or(app_id)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase();
    let hostname = format!("{slug}-{suffix}.{base_domain}");
    if suffix.is_empty() || hostname.len() > 253 {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_slug",
            "generated App ingress hostname is invalid",
        ));
    }
    Ok(hostname)
}

fn validate_ingress_return_path(value: &str) -> Result<String, AuthFailure> {
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.len() > 4096
        || value.chars().any(char::is_control)
    {
        return Err(AuthFailure::bad_request(
            "invalid_ingress_return_path",
            "ingress return path must be a local absolute path",
        ));
    }
    Ok(value.to_string())
}

fn resolved_service_ingress_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<ResolvedServiceIngress, AuthFailure> {
    let ingress = ServiceIngress {
        ingress_id: row.get("ingress_id"),
        workspace_id: row.get("workspace_id"),
        service_id: row.get("service_id"),
        hostname: row.get("hostname"),
        access: match row.get::<String, _>("access").as_str() {
            "public" => ServiceIngressAccess::Public,
            "workspace" => ServiceIngressAccess::Workspace,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("service ingress has invalid access mode {value}"),
                ))
            }
        },
        enabled: row.get("enabled"),
        created_at: parse_database_timestamp(&row, "created_at", "service ingress")?,
        created_by: row.get("created_by"),
        updated_at: parse_database_timestamp(&row, "updated_at", "service ingress")?,
        updated_by: row.get("updated_by"),
    };
    let target_port = u16::try_from(row.get::<i64, _>("target_port")).map_err(|error| {
        AuthFailure::internal(
            "database_error",
            format!("service ingress target has invalid port: {error}"),
        )
    })?;
    let service = MachineService {
        service_id: ingress.service_id.clone(),
        workspace_id: ingress.workspace_id.clone(),
        name: row.get("service_name"),
        server_id: row.get("server_id"),
        target_agent_id: row.get("target_agent_id"),
        target_host: row.get("target_host"),
        target_port,
        protocol: match row.get::<String, _>("service_protocol").as_str() {
            "tcp" => MachineServiceProtocol::Tcp,
            "http" => MachineServiceProtocol::Http,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("service ingress target has invalid protocol {value}"),
                ))
            }
        },
        created_at: parse_database_timestamp(&row, "service_created_at", "service ingress target")?,
        created_by: row.get("service_created_by"),
        updated_at: parse_database_timestamp(&row, "service_updated_at", "service ingress target")?,
        updated_by: row.get("service_updated_by"),
    };
    Ok(ResolvedServiceIngress { ingress, service })
}

pub(crate) fn normalize_virtual_hostname(value: &str) -> Result<String, AuthFailure> {
    let hostname = value.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels_valid = !hostname.is_empty()
        && hostname.len() <= 253
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if !labels_valid {
        return Err(AuthFailure::bad_request(
            "invalid_virtual_hostname",
            "hostname must contain valid DNS labels",
        ));
    }
    Ok(hostname)
}

fn virtual_network_host_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<VirtualNetworkHost, AuthFailure> {
    let created_at = row
        .get::<String, _>("created_at")
        .parse()
        .map_err(|error| {
            AuthFailure::internal(
                "database_error",
                format!("virtual network host has invalid created_at: {error}"),
            )
        })?;
    let target_port = row
        .get::<Option<i64>, _>("target_port")
        .map(u16::try_from)
        .transpose()
        .map_err(|_| {
            AuthFailure::internal(
                "database_error",
                "virtual network host has invalid target_port".to_string(),
            )
        })?;
    Ok(VirtualNetworkHost {
        workspace_id: row.get("workspace_id"),
        hostname: row.get("hostname"),
        service_id: row.get("service_id"),
        service_protocol: match row.get::<String, _>("service_protocol").as_str() {
            "tcp" => MachineServiceProtocol::Tcp,
            "http" => MachineServiceProtocol::Http,
            value => {
                return Err(AuthFailure::internal(
                    "database_error",
                    format!("virtual network host has invalid service protocol {value}"),
                ))
            }
        },
        destination_server_id: row.get("destination_server_id"),
        destination_agent_id: row.get("destination_agent_id"),
        target_host: row.get("target_host"),
        target_port,
        created_at,
        created_by: row.get("created_by"),
    })
}

fn hash_password(password: &str) -> Result<String, AuthFailure> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|error| AuthFailure::internal("password_hash_error", error.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| AuthFailure::internal("password_hash_error", error.to_string()))
}

fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

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
mod tests {
    use super::*;
    use axum::extract::Form;
    use axum::routing::{get, post};
    use axum::Router;
    use tokio::sync::{oneshot, Mutex};
    use treer_protocol::{AgentInfo, AgentStatus, CreateVirtualNetworkHostRequest, ServerStatus};

    type EmailCapture = Arc<Mutex<Option<oneshot::Sender<(HeaderMap, Value)>>>>;

    #[test]
    fn managed_app_ingress_hostnames_are_stable_and_dns_safe() {
        assert_eq!(
            managed_app_ingress_hostname("Soul Archive", "app_abcdef1234567890", "apps.treer.test")
                .expect("managed App hostname"),
            "soul-archive-abcdef123456.apps.treer.test"
        );
        assert_eq!(
            managed_app_ingress_hostname("灵魂", "app_1234567890abcdef", "apps.treer.test")
                .expect("fallback App hostname"),
            "app-1234567890ab.apps.treer.test"
        );
        assert!(managed_app_ingress_hostname("Soul", "app_---", "apps.treer.test").is_err());
    }

    async fn capture_email(
        State(capture): State<EmailCapture>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        if let Some(sender) = capture.lock().await.take() {
            let _ = sender.send((headers, body));
        }
        Json(json!({ "success": true }))
    }

    async fn oauth_token(Form(form): Form<HashMap<String, String>>) -> Json<Value> {
        assert_eq!(form.get("client_id").map(String::as_str), Some("client-id"));
        assert_eq!(
            form.get("client_secret").map(String::as_str),
            Some("client-secret")
        );
        assert_eq!(form.get("code").map(String::as_str), Some("oauth-code"));
        Json(json!({ "access_token": "provider-access-token" }))
    }

    fn assert_oauth_bearer(headers: &HeaderMap) {
        assert_eq!(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer provider-access-token")
        );
    }

    async fn github_user(headers: HeaderMap) -> Json<Value> {
        assert_oauth_bearer(&headers);
        Json(json!({ "id": 12345, "login": "octocat", "name": "Octo Cat" }))
    }

    async fn github_emails(headers: HeaderMap) -> Json<Value> {
        assert_oauth_bearer(&headers);
        Json(json!([
            { "email": "unverified@example.com", "primary": true, "verified": false },
            { "email": "octo@example.com", "primary": false, "verified": true }
        ]))
    }

    async fn google_user(headers: HeaderMap) -> Json<Value> {
        assert_oauth_bearer(&headers);
        Json(json!({
            "sub": "google-subject",
            "email": "google@example.com",
            "email_verified": true,
            "name": "Google User"
        }))
    }

    async fn bootstrap_owner(store: &AuthStore, email: &str, name: &str) -> CurrentSession {
        let (invite, _) = store
            .create_personal_invitation()
            .await
            .expect("personal invitation");
        store
            .register(Some(&invite), email, name, "password123")
            .await
            .expect("owner registration")
    }

    #[tokio::test]
    async fn app_oauth_codes_require_pkce_and_are_single_use() {
        let store = AuthStore::for_test("admin-password").await;
        store.seed_test_workspace("app-oauth").await;
        let owner = bootstrap_owner(&store, "ada@example.com", "Ada").await;
        sqlx::query(
            "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             VALUES($1, $2, 'owner', $3)",
        )
        .bind("org_app-oauth")
        .bind(&owner.user_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool)
        .await
        .expect("add app workspace member");
        sqlx::query(
            "INSERT INTO machine_services(\
             service_id, workspace_id, server_id, name, target_host, target_port, protocol, \
             created_at, created_by, updated_at, updated_by\
             ) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $8, $9)",
        )
        .bind("service-mail")
        .bind("app-oauth")
        .bind("machine-a")
        .bind("Mail")
        .bind("127.0.0.1")
        .bind(8788_i64)
        .bind("http")
        .bind(Utc::now().to_rfc3339())
        .bind("user-a")
        .execute(&store.pool)
        .await
        .expect("insert app service");
        let verifier = "v".repeat(64);
        let code = store
            .create_app_oauth_code(
                &AppOAuthGrant {
                    workspace_id: "app-oauth".to_string(),
                    service_id: "service-mail".to_string(),
                    user_id: owner.user_id.clone(),
                    preferred_name: "Ada".to_string(),
                    role: "owner".to_string(),
                },
                "https://mail.example/api/auth/callback",
                &pkce_challenge(&verifier),
            )
            .await
            .expect("create OAuth code");
        assert!(store
            .consume_app_oauth_code(
                &code,
                "service-mail",
                "https://mail.example/api/auth/callback",
                &"x".repeat(64),
            )
            .await
            .is_err());
        let grant = store
            .consume_app_oauth_code(
                &code,
                "service-mail",
                "https://mail.example/api/auth/callback",
                &verifier,
            )
            .await
            .expect("consume OAuth code");
        assert_eq!(grant.user_id, owner.user_id);
        assert!(store
            .consume_app_oauth_code(
                &code,
                "service-mail",
                "https://mail.example/api/auth/callback",
                &verifier,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn virtual_network_hosts_are_normalized_resolved_and_cleaned_up() {
        let mut store = AuthStore::for_test("owner-password").await;
        store.seed_test_workspace("default").await;
        let initial = store
            .virtual_network_hosts_snapshot("default")
            .await
            .expect("initial virtual-host snapshot");
        let service = store
            .create_machine_service(
                "default",
                "admin",
                CreateMachineServiceRequest {
                    name: "development API".to_string(),
                    server_id: "destination".to_string(),
                    target_agent_id: None,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 8080,
                    protocol: MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create machine service");
        let record = store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "API.Dev.Example.".to_string(),
                    service_id: service.service_id.clone(),
                },
            )
            .await
            .expect("create virtual host");
        let created = store
            .virtual_network_hosts_snapshot("default")
            .await
            .expect("created virtual-host snapshot");
        assert!(created.revision > initial.revision);
        assert_eq!(created.hosts, std::slice::from_ref(&record));
        assert_eq!(record.hostname, "api.dev.example");
        assert_eq!(record.service_id, service.service_id);
        assert_eq!(record.destination_server_id, "destination");
        assert_eq!(record.target_port, Some(8080));
        assert_eq!(
            store
                .resolve_virtual_network_host("default", "API.DEV.EXAMPLE")
                .await
                .expect("resolve virtual host"),
            Some(record.clone())
        );
        assert!(store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "api.dev.example".to_string(),
                    service_id: service.service_id.clone(),
                },
            )
            .await
            .is_err());
        store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "host.via.machine.treer".to_string(),
                    service_id: service.service_id.clone(),
                },
            )
            .await
            .expect("virtual host names have no reserved routing suffixes");
        store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "git.via.example".to_string(),
                    service_id: service.service_id.clone(),
                },
            )
            .await
            .expect("via label outside a Treer direct route is valid");
        store.disabled = true;
        store
            .delete_machine("default", "destination", &[])
            .await
            .expect("delete destination machine");
        let deleted = store
            .virtual_network_hosts_snapshot("default")
            .await
            .expect("deleted virtual-host snapshot");
        assert!(deleted.revision > created.revision);
        assert!(deleted.hosts.is_empty());
        assert!(store
            .list_machine_services("default")
            .await
            .expect("list machine services")
            .is_empty());
    }

    #[tokio::test]
    async fn machine_service_updates_refresh_aliases_and_delete_cascades() {
        let store = AuthStore::for_test("owner-password").await;
        store.seed_test_workspace("default").await;
        let service = store
            .create_machine_service(
                "default",
                "admin",
                CreateMachineServiceRequest {
                    name: "web".to_string(),
                    server_id: "machine-a".to_string(),
                    target_agent_id: None,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 3000,
                    protocol: MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create service");
        store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "web.internal".to_string(),
                    service_id: service.service_id.clone(),
                },
            )
            .await
            .expect("create alias");
        let updated = store
            .update_machine_service(
                "default",
                "web",
                "admin",
                UpdateMachineServiceRequest {
                    target_port: Some(4000),
                    ..UpdateMachineServiceRequest::default()
                },
            )
            .await
            .expect("update service");
        assert_eq!(updated.target_port, 4000);
        let alias = store
            .resolve_virtual_network_host("default", "web.internal")
            .await
            .expect("resolve alias")
            .expect("alias exists");
        assert_eq!(alias.target_port, Some(4000));

        store
            .delete_machine_service("default", &service.service_id)
            .await
            .expect("delete service");
        assert!(store
            .resolve_virtual_network_host("default", "web.internal")
            .await
            .expect("resolve deleted alias")
            .is_none());
    }

    #[tokio::test]
    async fn managed_app_owns_a_stable_service_and_virtual_host_across_runtime_restarts() {
        let store = AuthStore::for_test("owner-password").await;
        store.seed_test_workspace("apps").await;
        let app = store
            .create_app_deployment(
                "apps",
                "owner",
                "machine-a".to_string(),
                CreateAppDeploymentRequest {
                    server_id: Some("machine-a".to_string()),
                    name: "Soul".to_string(),
                    command: "python3".to_string(),
                    args: vec!["apps/soul/soul.py".to_string()],
                    cwd: ".".to_string(),
                    port: 9420,
                    hostname: "soul.internal".to_string(),
                },
            )
            .await
            .expect("create App deployment");
        let service = store
            .resolve_machine_service("apps", &app.service_id)
            .await
            .expect("resolve App service");
        assert_eq!(service.target_agent_id, None);
        assert_eq!(service.target_port, 9420);
        let host = store
            .resolve_virtual_network_host("apps", "soul.internal")
            .await
            .expect("resolve App virtual host")
            .expect("App virtual host");
        assert_eq!(host.service_id, app.service_id);
        let ingress = store
            .ensure_app_ingress(&app, "owner", "apps.treer.test")
            .await
            .expect("create App ingress");
        assert_eq!(ingress.service_id, app.service_id);
        assert_eq!(ingress.access, ServiceIngressAccess::Workspace);
        assert!(ingress.hostname.starts_with("soul-"));
        assert!(ingress.hostname.ends_with(".apps.treer.test"));
        assert_eq!(
            store
                .ensure_app_ingress(&app, "owner", "apps.treer.test")
                .await
                .expect("reuse App ingress")
                .ingress_id,
            ingress.ingress_id
        );

        let first = store
            .claim_app_runtime("apps", &app.app_id, None, "appw_first", "reconciler")
            .await
            .expect("claim first runtime")
            .expect("first runtime claim");
        assert_eq!(first.restart_count, 0);
        let second = store
            .claim_app_runtime(
                "apps",
                &app.app_id,
                Some("appw_first"),
                "appw_second",
                "reconciler",
            )
            .await
            .expect("replace runtime")
            .expect("replacement runtime claim");
        assert_eq!(second.restart_count, 1);
        assert_eq!(second.service_id, app.service_id);
        assert_eq!(second.hostname, "soul.internal");

        let stopped = store
            .set_app_desired_state("apps", &app.app_id, AppDesiredState::Stopped, "owner")
            .await
            .expect("stop App");
        assert_eq!(stopped.desired_state, AppDesiredState::Stopped);
        store
            .delete_app_deployment("apps", &app.app_id)
            .await
            .expect("delete App");
        assert!(store
            .resolve_machine_service("apps", &app.service_id)
            .await
            .is_err());
        assert!(store
            .resolve_virtual_network_host("apps", "soul.internal")
            .await
            .expect("resolve deleted host")
            .is_none());
        assert!(store
            .resolve_service_ingress_hostname(&ingress.hostname)
            .await
            .expect("resolve deleted ingress")
            .is_none());
    }

    #[tokio::test]
    async fn agent_services_keep_their_scope_and_delete_with_the_agent() {
        let store = AuthStore::for_test("owner-password").await;
        store.seed_test_workspace("default").await;
        let service = store
            .create_machine_service(
                "default",
                "agent:agent-a",
                CreateMachineServiceRequest {
                    name: "Agent app".to_string(),
                    server_id: "machine-a".to_string(),
                    target_agent_id: Some("agent-a".to_string()),
                    target_host: "localhost".to_string(),
                    target_port: 3000,
                    protocol: MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create Agent service");
        assert_eq!(service.target_agent_id.as_deref(), Some("agent-a"));
        assert_eq!(service.target_host, "127.0.0.1");

        let host = store
            .create_virtual_network_host(
                "default",
                "agent:agent-a",
                CreateVirtualNetworkHostRequest {
                    hostname: "agent-app.internal".to_string(),
                    service_id: service.service_id.clone(),
                },
            )
            .await
            .expect("create Agent virtual host");
        assert_eq!(host.destination_agent_id.as_deref(), Some("agent-a"));

        let error = store
            .update_machine_service(
                "default",
                &service.service_id,
                "agent:agent-a",
                UpdateMachineServiceRequest {
                    server_id: Some("machine-b".to_string()),
                    ..UpdateMachineServiceRequest::default()
                },
            )
            .await
            .expect_err("Agent service scope must be immutable");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        store
            .delete_agent("default", "agent-a")
            .await
            .expect("delete Agent");
        store
            .refresh_virtual_network_hosts()
            .await
            .expect("refresh virtual hosts");
        assert!(store
            .resolve_machine_service("default", &service.service_id)
            .await
            .is_err());
        assert!(store
            .virtual_network_hosts_snapshot("default")
            .await
            .expect("virtual hosts")
            .hosts
            .is_empty());
    }

    #[tokio::test]
    async fn agent_launch_profiles_support_crud_validation_and_audit() {
        let store = AuthStore::for_test("owner-password").await;
        store.seed_test_workspace("profiles").await;

        let created = store
            .create_agent_launch_profile(
                "profiles",
                ProfileMutationActor {
                    kind: "agent",
                    id: Some("agent-owner"),
                    label: "agent:agent-owner",
                },
                CreateAgentLaunchProfileRequest {
                    name: "Reviewer".to_string(),
                    description: "Review the current change".to_string(),
                    cwd: ".".to_string(),
                    command: "codex".to_string(),
                    args: vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
                },
            )
            .await
            .expect("create launch profile");
        assert!(created.profile_id.starts_with("alp_"));
        assert_eq!(created.created_by, "agent:agent-owner");
        assert_eq!(
            store
                .resolve_agent_launch_profile("profiles", "reviewer")
                .await
                .expect("resolve launch profile by name"),
            created
        );
        store.seed_test_workspace("other-workspace").await;
        let cross_workspace = store
            .resolve_agent_launch_profile("other-workspace", &created.profile_id)
            .await
            .expect_err("profiles cannot be resolved across workspaces");
        assert_eq!(cross_workspace.error.code, "launch_profile_not_found");

        let duplicate = store
            .create_agent_launch_profile(
                "profiles",
                ProfileMutationActor {
                    kind: "agent",
                    id: Some("agent-owner"),
                    label: "agent:agent-owner",
                },
                CreateAgentLaunchProfileRequest {
                    name: "REVIEWER".to_string(),
                    description: String::new(),
                    cwd: String::new(),
                    command: "claude".to_string(),
                    args: Vec::new(),
                },
            )
            .await
            .expect_err("profile names are unique within a workspace");
        assert_eq!(duplicate.error.code, "launch_profile_exists");

        let updated = store
            .update_agent_launch_profile(
                "profiles",
                &created.profile_id,
                ProfileMutationActor {
                    kind: "user",
                    id: Some("user-owner"),
                    label: "user-owner",
                },
                UpdateAgentLaunchProfileRequest {
                    name: Some("Code reviewer".to_string()),
                    args: Some(vec![
                        "review".to_string(),
                        "--base".to_string(),
                        "main".to_string(),
                    ]),
                    ..UpdateAgentLaunchProfileRequest::default()
                },
            )
            .await
            .expect("update launch profile");
        assert_eq!(updated.name, "Code reviewer");
        assert_eq!(updated.args, ["review", "--base", "main"]);
        assert_eq!(updated.updated_by, "user-owner");

        let invalid = store
            .update_agent_launch_profile(
                "profiles",
                &created.profile_id,
                ProfileMutationActor {
                    kind: "user",
                    id: Some("user-owner"),
                    label: "user-owner",
                },
                UpdateAgentLaunchProfileRequest {
                    args: Some(vec!["bad\0argument".to_string()]),
                    ..UpdateAgentLaunchProfileRequest::default()
                },
            )
            .await
            .expect_err("NUL bytes are rejected");
        assert_eq!(invalid.error.code, "invalid_launch_profile");

        store
            .delete_agent_launch_profile(
                "profiles",
                &created.profile_id,
                ProfileMutationActor {
                    kind: "user",
                    id: Some("user-owner"),
                    label: "user-owner",
                },
            )
            .await
            .expect("delete launch profile");
        assert!(store
            .list_agent_launch_profiles("profiles")
            .await
            .expect("list launch profiles")
            .is_empty());

        let actions = sqlx::query_scalar::<_, String>(
            "SELECT action FROM organization_audit_events WHERE workspace_id = $1 ORDER BY sequence",
        )
        .bind("profiles")
        .fetch_all(&store.pool)
        .await
        .expect("list audit actions");
        assert_eq!(
            actions,
            [
                "launch_profile.created",
                "launch_profile.updated",
                "launch_profile.deleted"
            ]
        );
    }

    #[tokio::test]
    async fn schema_initialization_is_idempotent() {
        let store = AuthStore::for_test("owner-password").await;
        store
            .initialize_schema()
            .await
            .expect("repeat schema initialization");
        assert_eq!(
            store.all_workspaces().await.expect("load workspaces").len(),
            0
        );
    }

    #[tokio::test]
    async fn invitation_registration_and_login_round_trip() {
        let store = AuthStore::for_test("owner-password").await;
        let admin = store
            .admin_login("owner-password")
            .await
            .expect("admin login");
        assert!(store.admin_session(&admin.token).await.unwrap().is_some());
        let (invite, url) = store
            .create_personal_invitation()
            .await
            .expect("invitation");
        assert!(url.as_str().contains(&invite));
        assert!(url
            .as_str()
            .starts_with("https://app.treer.example/?invite="));

        let registered = store
            .register(Some(&invite), "Alice@Example.com", "Alice", "password123")
            .await
            .expect("registration");
        assert_eq!(registered.email, "alice@example.com");
        assert_eq!(registered.preferred_name, "Alice");
        let organizations = store
            .list_organizations(&registered.user_id)
            .await
            .expect("personal organization");
        assert_eq!(organizations.len(), 1);
        assert_eq!(organizations[0].name, "Alice Personal");
        assert_eq!(organizations[0].role, "owner");
        assert!(store
            .register(Some(&invite), "bob@example.com", "Bob", "password123")
            .await
            .is_err());

        let login = store
            .login("ALICE@EXAMPLE.COM", "password123")
            .await
            .expect("case-insensitive login");
        assert_eq!(login.email, "alice@example.com");
        let updated = store
            .update_profile(&login.user_id, "alicia@example.com", "Alicia")
            .await
            .expect("update profile");
        assert_eq!(updated.preferred_name, "Alicia");
        assert!(store
            .login("alice@example.com", "password123")
            .await
            .is_err());
        assert!(store
            .login("alicia@example.com", "password123")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn successful_registration_sends_a_welcome_email() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind email API");
        let address = listener.local_addr().expect("email API address");
        let (capture_tx, capture_rx) = oneshot::channel();
        let capture = Arc::new(Mutex::new(Some(capture_tx)));
        let app = Router::new()
            .route("/send", post(capture_email))
            .with_state(capture);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let mut store = AuthStore::for_test("owner-password").await;
        store.email_sender = Some(CloudflareEmailSender {
            client: reqwest::Client::new(),
            endpoint: Url::parse(&format!("http://{address}/send")).expect("email endpoint"),
            api_token: "cloudflare-test-token".into(),
            from: "service@treer.ai".into(),
        });
        let (invite, _) = store
            .create_personal_invitation()
            .await
            .expect("invitation");
        store
            .register(Some(&invite), "Alice@Example.com", "Alice", "password123")
            .await
            .expect("registration");

        let (headers, body) = tokio::time::timeout(StdDuration::from_secs(2), capture_rx)
            .await
            .expect("welcome email timeout")
            .expect("captured welcome email");
        assert_eq!(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer cloudflare-test-token")
        );
        assert_eq!(body["to"], "alice@example.com");
        assert_eq!(body["from"], "service@treer.ai");
        assert_eq!(body["subject"], "Welcome to Treer");
        assert!(body["text"].as_str().expect("text body").contains("Alice"));
        assert!(body["html"].as_str().expect("HTML body").contains("Alice"));
        server.abort();
    }

    #[tokio::test]
    async fn oauth_provider_exchange_uses_verified_provider_profiles() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind OAuth provider");
        let address = listener.local_addr().expect("OAuth provider address");
        let base = format!("http://{address}");
        let app = Router::new()
            .route("/token", post(oauth_token))
            .route("/github/user", get(github_user))
            .route("/github/emails", get(github_emails))
            .route("/google/user", get(google_user));
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let github = OAuthProviderConfig::new(
            "client-id".to_string(),
            "client-secret".to_string(),
            &format!("{base}/authorize"),
            &format!("{base}/token"),
            &format!("{base}/github/user"),
            Some(&format!("{base}/github/emails")),
        )
        .expect("GitHub OAuth config");
        let google = OAuthProviderConfig::new(
            "client-id".to_string(),
            "client-secret".to_string(),
            &format!("{base}/authorize"),
            &format!("{base}/token"),
            &format!("{base}/google/user"),
            None,
        )
        .expect("Google OAuth config");
        let mut store = AuthStore::for_test("owner-password").await;
        store.oauth = Arc::new(OAuthConfig::new(Some(github), Some(google), true));

        let github = store
            .exchange_oauth_code("github", "oauth-code")
            .await
            .expect("GitHub profile");
        assert_eq!(github.subject, "12345");
        assert_eq!(github.email, "octo@example.com");
        assert_eq!(github.preferred_name, "Octo Cat");
        let google = store
            .exchange_oauth_code("google", "oauth-code")
            .await
            .expect("Google profile");
        assert_eq!(google.subject, "google-subject");
        assert_eq!(google.email, "google@example.com");
        assert_eq!(google.preferred_name, "Google User");
        server.abort();
    }

    #[tokio::test]
    async fn oauth_state_is_provider_scoped_and_single_use() {
        let github =
            OAuthProviderConfig::github("client-id".to_string(), "client-secret".to_string())
                .expect("GitHub OAuth config");
        let mut store = AuthStore::for_test("owner-password").await;
        store.oauth = Arc::new(OAuthConfig::new(Some(github), None, true));
        let authorization = store
            .oauth_authorization_url("github", Some("invite-token"))
            .await
            .expect("authorization URL");
        assert_eq!(authorization.host_str(), Some("github.com"));
        assert!(authorization.query_pairs().any(|(key, value)| {
            key == "redirect_uri"
                && value == "https://proxy.treer.example/api/auth/oauth/github/callback"
        }));
        let state = authorization
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
            .expect("OAuth state");
        assert!(store.consume_oauth_state("google", &state).await.is_err());
        assert_eq!(
            store
                .consume_oauth_state("github", &state)
                .await
                .expect("consume state")
                .as_deref(),
            Some("invite-token")
        );
        assert!(store.consume_oauth_state("github", &state).await.is_err());
    }

    #[tokio::test]
    async fn oauth_merges_verified_email_and_keeps_stable_provider_identity() {
        let store = AuthStore::for_test("owner-password").await;
        let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
        let merged = store
            .complete_oauth_login(
                OAuthProfile {
                    provider: "github",
                    subject: "github-123".to_string(),
                    email: "OWNER@example.com".to_string(),
                    preferred_name: "Provider Name".to_string(),
                },
                None,
            )
            .await
            .expect("merge by verified email");
        assert_eq!(merged.user_id, owner.user_id);
        assert_eq!(merged.preferred_name, "Owner");
        assert!(store
            .session(&owner.token)
            .await
            .expect("old session")
            .is_none());
        assert!(store
            .login("owner@example.com", "password123")
            .await
            .is_err());

        let stable = store
            .complete_oauth_login(
                OAuthProfile {
                    provider: "github",
                    subject: "github-123".to_string(),
                    email: "changed@example.com".to_string(),
                    preferred_name: "Changed Provider Name".to_string(),
                },
                None,
            )
            .await
            .expect("login by stable provider identity");
        assert_eq!(stable.user_id, owner.user_id);
        assert_eq!(stable.email, "owner@example.com");
        let identity_user: String = sqlx::query_scalar(
            "SELECT user_id FROM oauth_identities WHERE provider = 'github' AND subject = $1",
        )
        .bind("github-123")
        .fetch_one(&store.pool)
        .await
        .expect("linked identity");
        assert_eq!(identity_user, owner.user_id);
    }

    #[tokio::test]
    async fn invitation_switch_controls_new_password_and_oauth_accounts() {
        let mut required = AuthStore::for_test("owner-password").await;
        assert!(required
            .complete_oauth_login(
                OAuthProfile {
                    provider: "google",
                    subject: "new-google-user".to_string(),
                    email: "new@example.com".to_string(),
                    preferred_name: "New User".to_string(),
                },
                None,
            )
            .await
            .is_err());

        required.oauth = Arc::new(OAuthConfig::new(None, None, false));
        let oauth_user = required
            .complete_oauth_login(
                OAuthProfile {
                    provider: "google",
                    subject: "new-google-user".to_string(),
                    email: "new@example.com".to_string(),
                    preferred_name: "New User".to_string(),
                },
                None,
            )
            .await
            .expect("OAuth registration without invite");
        assert_eq!(
            required
                .list_organizations(&oauth_user.user_id)
                .await
                .expect("OAuth personal organization")[0]
                .name,
            "New User Personal"
        );
        let password_user = required
            .register(None, "password@example.com", "Password User", "password123")
            .await
            .expect("password registration without invite");
        assert_eq!(
            required
                .list_organizations(&password_user.user_id)
                .await
                .expect("password personal organization")[0]
                .name,
            "Password User Personal"
        );
    }

    #[tokio::test]
    async fn password_reset_is_single_use_rate_limited_and_revokes_sessions() {
        let store = AuthStore::for_test("owner-password").await;
        let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
        let pending = store
            .create_password_reset("OWNER@example.com")
            .await
            .expect("create password reset")
            .expect("known user reset");
        assert!(pending
            .url
            .as_str()
            .starts_with("https://app.treer.example/?reset="));
        assert!(store
            .create_password_reset("owner@example.com")
            .await
            .expect("rate limit reset")
            .is_none());
        assert!(store
            .create_password_reset("missing@example.com")
            .await
            .expect("unknown email")
            .is_none());

        let token = pending
            .url
            .query_pairs()
            .find_map(|(key, value)| (key == "reset").then(|| value.into_owned()))
            .expect("reset token in URL");
        let stored_hash = sqlx::query_scalar::<_, String>(
            "SELECT secret_hash FROM password_reset_tokens WHERE token_id = $1",
        )
        .bind(&pending.token_id)
        .fetch_one(&store.pool)
        .await
        .expect("stored reset hash");
        assert!(!stored_hash.contains(&token));

        store
            .reset_password(&token, "new-password-123")
            .await
            .expect("reset password");
        assert!(store
            .session(&owner.token)
            .await
            .expect("read old session")
            .is_none());
        assert!(store
            .login("owner@example.com", "password123")
            .await
            .is_err());
        assert!(store
            .login("owner@example.com", "new-password-123")
            .await
            .is_ok());
        assert!(store
            .reset_password(&token, "another-password")
            .await
            .is_err());

        let pending = store
            .create_password_reset("owner@example.com")
            .await
            .expect("create second password reset")
            .expect("second reset");
        let token = pending
            .url
            .query_pairs()
            .find_map(|(key, value)| (key == "reset").then(|| value.into_owned()))
            .expect("second reset token in URL");
        store
            .update_profile(&owner.user_id, "renamed@example.com", "Owner")
            .await
            .expect("change account email");
        assert!(store
            .reset_password(&token, "newer-password")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn cloudflare_password_reset_email_uses_structured_send_api() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind email API");
        let address = listener.local_addr().expect("email API address");
        let (capture_tx, capture_rx) = oneshot::channel();
        let capture = Arc::new(Mutex::new(Some(capture_tx)));
        let app = Router::new()
            .route("/send", post(capture_email))
            .with_state(capture);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let sender = CloudflareEmailSender {
            client: reqwest::Client::new(),
            endpoint: Url::parse(&format!("http://{address}/send")).expect("email endpoint"),
            api_token: "cloudflare-test-token".into(),
            from: "service@treer.ai".into(),
        };
        let reset_url = Url::parse(
            "https://app.treer.example/?reset=pwd_0123456789abcdef0123456789abcdef.secret&source=test",
        )
        .expect("reset URL");
        sender
            .send_password_reset("owner@example.com", &reset_url)
            .await
            .expect("send reset email");
        let (headers, body) = capture_rx.await.expect("captured email");
        assert_eq!(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer cloudflare-test-token")
        );
        assert_eq!(body["to"], "owner@example.com");
        assert_eq!(body["from"], "service@treer.ai");
        assert_eq!(body["subject"], "Reset your Treer password");
        assert!(body["text"]
            .as_str()
            .expect("text body")
            .contains(reset_url.as_str()));
        assert!(body["html"]
            .as_str()
            .expect("HTML body")
            .contains("&amp;source=test"));
        server.abort();
    }

    #[tokio::test]
    async fn new_workspaces_include_deletable_default_launch_profiles() {
        let store = AuthStore::for_test("owner-password").await;
        let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
        let organization = store
            .list_organizations(&owner.user_id)
            .await
            .expect("list organizations")
            .remove(0);
        store
            .create_workspace(
                &organization.organization_id,
                "defaults",
                "Defaults",
                &owner.user_id,
            )
            .await
            .expect("create workspace");

        let profiles = store
            .list_agent_launch_profiles("defaults")
            .await
            .expect("list default profiles");
        assert_eq!(
            profiles
                .iter()
                .map(|profile| (profile.name.as_str(), profile.command.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Claude", "claude"),
                ("Codex", "codex"),
                ("OpenCode", "opencode"),
                ("Pi", "pi"),
            ]
        );
        assert!(profiles.iter().all(|profile| {
            profile.cwd == "." && profile.args.is_empty() && profile.created_by == owner.user_id
        }));

        store
            .delete_agent_launch_profile(
                "defaults",
                "Codex",
                ProfileMutationActor {
                    kind: "user",
                    id: Some(&owner.user_id),
                    label: &owner.preferred_name,
                },
            )
            .await
            .expect("default profile remains deletable");
        assert_eq!(
            store
                .list_agent_launch_profiles("defaults")
                .await
                .expect("list profiles after delete")
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn workspace_deletion_requires_a_manager_and_revokes_credentials() {
        let store = AuthStore::for_test("owner-password").await;
        let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
        let organization = store
            .create_organization(&owner.user_id, "Engineering")
            .await
            .expect("create organization");
        store
            .create_workspace(
                &organization.organization_id,
                "ws_deletable",
                "Deletable",
                &owner.user_id,
            )
            .await
            .expect("create workspace");
        let (invite, _) = store
            .create_invitation(&organization.organization_id, &owner.user_id)
            .await
            .expect("invite member");
        let member = store
            .register(Some(&invite), "member@example.com", "Member", "password123")
            .await
            .expect("register member");
        assert!(
            store
                .delete_workspace("ws_deletable", &member.user_id)
                .await
                .is_err(),
            "a plain member cannot delete a workspace"
        );

        let enrollment = store
            .create_machine_enrollment("ws_deletable", &owner.user_id)
            .await
            .expect("create enrollment");
        let machine = store
            .claim_machine_enrollment(&enrollment)
            .await
            .expect("claim machine");
        sqlx::query(
            "INSERT INTO agent_credentials(agent_id, workspace_id, server_id, secret_hash, created_at) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind("agent-1")
        .bind("ws_deletable")
        .bind(&machine.server_id)
        .bind("agent-secret-hash")
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool)
        .await
        .expect("insert agent credential");
        sqlx::query(
            "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4)",
        )
        .bind(&machine.server_id)
        .bind("ws_deletable")
        .bind("machine name")
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool)
        .await
        .expect("insert machine name");
        sqlx::query(
            "INSERT INTO agent_names(agent_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4)",
        )
        .bind("agent-1")
        .bind("ws_deletable")
        .bind("agent name")
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool)
        .await
        .expect("insert agent name");
        store
            .create_agent_launch_profile(
                "ws_deletable",
                ProfileMutationActor {
                    kind: "user",
                    id: Some(&owner.user_id),
                    label: &owner.preferred_name,
                },
                CreateAgentLaunchProfileRequest {
                    name: "Custom".to_string(),
                    description: "".to_string(),
                    cwd: ".".to_string(),
                    command: "bash".to_string(),
                    args: vec![],
                },
            )
            .await
            .expect("create launch profile");

        sqlx::query(
            "INSERT INTO machine_traffic_hourly(\
             workspace_id, window_start, source_server_id, destination_server_id, \
             payload_bytes, payload_frames, updated_at) VALUES($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind("ws_deletable")
        .bind(Utc::now().timestamp())
        .bind(&machine.server_id)
        .bind("peer-machine")
        .bind(128_i64)
        .bind(1_i64)
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool)
        .await
        .expect("insert traffic history");
        crate::message_store::MessageStore::open(store.pool())
            .await
            .expect("initialize message store");
        sqlx::query(
            "INSERT INTO core_messages(\
             message_id, workspace_id, sender_kind, sender_id, sender_name, body, created_at) \
             VALUES($1, $2, 'agent', $3, $4, $5, $6)",
        )
        .bind("msg-history")
        .bind("ws_deletable")
        .bind("agent-1")
        .bind("Agent One")
        .bind("retained history")
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool)
        .await
        .expect("insert message history");
        let unused_enrollment = store
            .create_machine_enrollment("ws_deletable", &owner.user_id)
            .await
            .expect("create unused enrollment");

        let blocked = store
            .delete_workspace("ws_deletable", &owner.user_id)
            .await
            .expect_err("an active machine must block workspace deletion");
        let (status, error) = blocked.into_parts();
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error.code, "workspace_has_machines");

        store
            .delete_machine("ws_deletable", &machine.server_id, &["agent-1".to_string()])
            .await
            .expect("delete machine first");

        let deleted = store
            .delete_workspace("ws_deletable", &owner.user_id)
            .await
            .expect("delete workspace");
        assert_eq!(deleted.workspace_id, "ws_deletable");
        assert_eq!(deleted.organization_id, organization.organization_id);
        assert_eq!(deleted.name, "Deletable");
        assert_eq!(deleted.machine_count, 0);
        assert_eq!(deleted.agent_count, 1);
        assert_eq!(deleted.app_count, 0);

        assert!(store
            .list_workspaces(&organization.organization_id, &owner.user_id)
            .await
            .expect("list workspaces")
            .is_empty());
        let retained = sqlx::query(
            "SELECT deleted_at, deleted_by, \
             (SELECT COUNT(*) FROM machines WHERE workspace_id = $1) AS machines, \
             (SELECT COUNT(*) FROM agent_credentials WHERE workspace_id = $1) AS agents, \
             (SELECT COUNT(*) FROM agent_launch_profiles WHERE workspace_id = $1) AS profiles, \
             (SELECT COUNT(*) FROM machine_traffic_hourly WHERE workspace_id = $1) AS traffic, \
             (SELECT COUNT(*) FROM core_messages WHERE workspace_id = $1) AS messages \
             FROM workspaces WHERE workspace_id = $1",
        )
        .bind("ws_deletable")
        .fetch_one(&store.pool)
        .await
        .expect("load retained workspace history");
        assert!(retained.get::<Option<String>, _>("deleted_at").is_some());
        assert_eq!(
            retained.get::<Option<String>, _>("deleted_by"),
            Some(owner.user_id.clone())
        );
        assert_eq!(retained.get::<i64, _>("machines"), 1);
        assert_eq!(retained.get::<i64, _>("agents"), 1);
        assert_eq!(retained.get::<i64, _>("profiles"), 5);
        assert_eq!(retained.get::<i64, _>("traffic"), 1);
        assert_eq!(retained.get::<i64, _>("messages"), 1);
        assert!(store
            .claim_machine_enrollment(&unused_enrollment)
            .await
            .is_err());
        assert!(
            store
                .delete_workspace("ws_deletable", &owner.user_id)
                .await
                .is_err(),
            "a deleted workspace cannot be deleted again"
        );
        let audit_events = store
            .list_audit_events(
                &organization.organization_id,
                &owner.user_id,
                None,
                None,
                100,
            )
            .await
            .expect("owner audit events");
        let deletion = audit_events
            .iter()
            .find(|event| event.action == "workspace.deleted")
            .expect("workspace.deleted audit event");
        assert_eq!(deletion.resource_id, "ws_deletable");
        assert_eq!(deletion.payload["machine_count"], 0);
        assert_eq!(deletion.payload["agent_count"], 1);
    }

    #[tokio::test]
    async fn organization_roles_control_members_and_share_workspaces() {
        let store = AuthStore::for_test("owner-password").await;
        let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
        let organization = store
            .create_organization(&owner.user_id, "Engineering")
            .await
            .expect("create organization");
        let renamed = store
            .rename_organization(&organization.organization_id, &owner.user_id, "Product")
            .await
            .expect("rename organization");
        assert_eq!(renamed.name, "Product");
        store
            .create_workspace(
                &organization.organization_id,
                "ws_engineering",
                "Engineering",
                &owner.user_id,
            )
            .await
            .expect("create workspace");
        let (alice_invite, _) = store
            .create_invitation(&organization.organization_id, &owner.user_id)
            .await
            .expect("invite alice");
        let alice = store
            .register(
                Some(&alice_invite),
                "alice@example.com",
                "Alice",
                "password123",
            )
            .await
            .expect("register alice");
        let alice_organizations = store
            .list_organizations(&alice.user_id)
            .await
            .expect("alice organizations");
        assert_eq!(alice_organizations.len(), 1);
        assert_eq!(
            alice_organizations[0].organization_id,
            organization.organization_id
        );
        assert_ne!(alice_organizations[0].name, "Alice Personal");

        let workspaces = store
            .list_workspaces(&organization.organization_id, &alice.user_id)
            .await
            .expect("member workspaces");
        assert_eq!(workspaces[0].workspace_id, "ws_engineering");
        let renamed_workspace = store
            .rename_workspace("ws_engineering", &alice.user_id, "Platform")
            .await
            .expect("members may rename workspaces");
        assert_eq!(renamed_workspace.workspace_id, "ws_engineering");
        assert_eq!(renamed_workspace.name, "Platform");
        assert_eq!(
            store
                .list_workspaces(&organization.organization_id, &alice.user_id)
                .await
                .expect("renamed workspace remains visible")[0]
                .name,
            "Platform"
        );
        store
            .create_workspace(
                &organization.organization_id,
                "ws_product",
                "Product",
                &alice.user_id,
            )
            .await
            .expect("members may create workspaces");
        assert!(store
            .create_invitation(&organization.organization_id, &alice.user_id)
            .await
            .is_err());
        assert!(store
            .list_audit_events(
                &organization.organization_id,
                &alice.user_id,
                Some("ws_engineering"),
                None,
                100,
            )
            .await
            .is_err());

        store
            .update_member_role(
                &organization.organization_id,
                &owner.user_id,
                &alice.user_id,
                "admin",
            )
            .await
            .expect("promote alice");
        let (bob_invite, _) = store
            .create_invitation(&organization.organization_id, &alice.user_id)
            .await
            .expect("admin invite");
        let bob = store
            .register(Some(&bob_invite), "bob@example.com", "Bob", "password123")
            .await
            .expect("register bob");
        store
            .remove_member(&organization.organization_id, &alice.user_id, &bob.user_id)
            .await
            .expect("admin removes member");
        assert!(store
            .remove_member(
                &organization.organization_id,
                &alice.user_id,
                &owner.user_id
            )
            .await
            .is_err());
        let audit_events = store
            .list_audit_events(
                &organization.organization_id,
                &owner.user_id,
                Some("ws_engineering"),
                None,
                100,
            )
            .await
            .expect("owner audit events");
        assert!(audit_events
            .iter()
            .any(|event| event.action == "organization.renamed"));
        assert!(audit_events
            .iter()
            .any(|event| event.action == "workspace.created"));
        assert!(audit_events.iter().any(|event| {
            event.action == "workspace.renamed"
                && event.payload["old_name"] == "Engineering"
                && event.payload["new_name"] == "Platform"
        }));
        assert!(audit_events
            .iter()
            .any(|event| event.action == "member.role_updated"));
        assert!(!serde_json::to_string(&audit_events)
            .expect("serialize audit events")
            .contains(&alice_invite));
    }

    #[tokio::test]
    async fn workspace_access_is_limited_to_organization_members() {
        let store = AuthStore::for_test("owner-password").await;
        let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
        let personal = store
            .list_organizations(&owner.user_id)
            .await
            .expect("owner organizations")
            .into_iter()
            .next()
            .expect("personal organization");
        let (invite, _) = store
            .create_invitation(&personal.organization_id, &owner.user_id)
            .await
            .expect("personal organization invite");
        let alice = store
            .register(Some(&invite), "alice@example.com", "Alice", "password123")
            .await
            .expect("register alice");
        let private = store
            .create_organization(&owner.user_id, "Private")
            .await
            .expect("create private organization");
        store
            .create_workspace(
                &private.organization_id,
                "ws_private",
                "Private",
                &owner.user_id,
            )
            .await
            .expect("create private workspace");

        let error = store
            .require_workspace_member("ws_private", &alice.user_id)
            .await
            .expect_err("cross-organization access must fail");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn service_ingress_and_human_authorization_are_durable_and_scoped() {
        let store = AuthStore::for_test("owner-password").await;
        let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
        let organization = store
            .list_organizations(&owner.user_id)
            .await
            .expect("list organizations")
            .into_iter()
            .next()
            .expect("personal organization");
        store
            .create_workspace(
                &organization.organization_id,
                "published",
                "Published",
                &owner.user_id,
            )
            .await
            .expect("create workspace");
        let service = store
            .create_machine_service(
                "published",
                &owner.user_id,
                CreateMachineServiceRequest {
                    name: "Issue Tracker".to_string(),
                    server_id: "machine-a".to_string(),
                    target_agent_id: None,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 3000,
                    protocol: MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create service");
        let ingress = store
            .create_service_ingress(
                "published",
                &owner.user_id,
                "apps.treer.ai",
                CreateServiceIngressRequest {
                    service_id: service.service_id.clone(),
                    slug: None,
                    access: ServiceIngressAccess::Workspace,
                },
            )
            .await
            .expect("create ingress");
        assert!(ingress.hostname.starts_with("issue-tracker-"));
        assert!(ingress.hostname.ends_with(".apps.treer.ai"));
        assert_eq!(
            store
                .resolve_service_ingress_hostname(&ingress.hostname)
                .await
                .expect("resolve ingress")
                .expect("stored ingress")
                .service
                .service_id,
            service.service_id
        );

        let code = store
            .create_ingress_auth_code(&ingress, &owner.user_id, "/issues?mine=1")
            .await
            .expect("create authorization code");
        let authorization = store
            .consume_ingress_auth_code(&ingress.hostname, &code)
            .await
            .expect("consume authorization code");
        assert_eq!(authorization.return_path, "/issues?mine=1");
        assert_eq!(
            store
                .authenticate_ingress_session(&ingress.hostname, &authorization.session_token)
                .await
                .expect("authenticate ingress session")
                .as_deref(),
            Some(owner.user_id.as_str())
        );
        assert!(store
            .consume_ingress_auth_code(&ingress.hostname, &code)
            .await
            .is_err());

        let disabled = store
            .update_service_ingress(
                "published",
                &ingress.ingress_id,
                &owner.user_id,
                UpdateServiceIngressRequest {
                    access: None,
                    enabled: Some(false),
                },
            )
            .await
            .expect("disable ingress");
        assert!(!disabled.enabled);
        assert!(store
            .authenticate_ingress_session(&ingress.hostname, &authorization.session_token)
            .await
            .expect("disabled session lookup")
            .is_none());
    }

    #[tokio::test]
    async fn logout_invalidates_the_session() {
        let store = AuthStore::for_test("owner-password").await;
        let session = bootstrap_owner(&store, "owner@example.com", "Owner").await;
        assert!(store
            .session(&session.token)
            .await
            .expect("session lookup")
            .is_some());
        store.logout(&session.token).await.expect("logout");
        assert!(store
            .session(&session.token)
            .await
            .expect("session lookup")
            .is_none());
    }

    #[test]
    fn cookie_parser_handles_multiple_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; treer_session=abc123; other=value"),
        );
        assert_eq!(
            cookie_value(&headers, SESSION_COOKIE).as_deref(),
            Some("abc123")
        );
    }

    #[tokio::test]
    async fn disabled_auth_injects_a_local_user() {
        let mut store = AuthStore::for_test("owner-password").await;
        store.disabled = true;
        let session = authenticate_request(&store, &HeaderMap::new())
            .await
            .expect("local session");
        assert_eq!(session.user_id, "local");
        assert_eq!(session.preferred_name, "Local user");
    }

    #[tokio::test]
    async fn machine_enrollment_is_single_use_and_binds_identity() {
        let store = AuthStore::for_test("owner-password").await;
        let enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create enrollment");
        assert_eq!(
            parse_machine_enrollment_key(&enrollment)
                .expect("parse enrollment")
                .workspace_id,
            "workspace-a"
        );
        let claim = store
            .claim_machine_enrollment(&enrollment)
            .await
            .expect("claim enrollment");
        assert_eq!(claim.workspace_id, "workspace-a");
        assert!(claim.server_id.starts_with("srv_"));
        assert!(store.claim_machine_enrollment(&enrollment).await.is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", claim.machine_token))
                .expect("authorization header"),
        );
        let machine = store
            .authenticate_machine(&headers)
            .await
            .expect("authenticate machine");
        assert!(machine.allows_server("workspace-a", &claim.server_id));
        assert!(!machine.allows_server("workspace-b", &claim.server_id));
        assert!(!machine.allows_server("workspace-a", "srv_other"));

        let workload_credential = store
            .create_agent_credential("workspace-a", &claim.server_id, "agent-a")
            .await
            .expect("create Agent credential");
        headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
        headers.insert(
            WORKLOAD_CREDENTIAL_HEADER,
            HeaderValue::from_str(&workload_credential).expect("workload header"),
        );
        let agent = store
            .authenticate_agent(&machine, &headers)
            .await
            .expect("authenticate Agent")
            .expect("Agent session");
        assert_eq!(agent.server_id, claim.server_id);

        let other_machine = MachineSession {
            server_id: Some("srv_other".to_string()),
            workspace_id: Some("workspace-a".to_string()),
        };
        assert!(store
            .authenticate_agent(&other_machine, &headers)
            .await
            .is_err());

        headers.insert(
            WORKLOAD_CREDENTIAL_HEADER,
            HeaderValue::from_static("wlc_invalid"),
        );
        assert!(store.authenticate_agent(&machine, &headers).await.is_err());
    }

    #[tokio::test]
    async fn repeated_enrollment_reuses_installation_identity_and_rotates_credentials() {
        let store = AuthStore::for_test("owner-password").await;
        let installation_id = "mid_0123456789abcdef0123456789abcdef";
        let first_enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create first enrollment");
        let first = store
            .claim_machine_enrollment_for_installation(
                &first_enrollment,
                Some(installation_id),
                Some("Builder one"),
                None,
            )
            .await
            .expect("claim first enrollment");
        let second_enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create second enrollment");
        let second = store
            .claim_machine_enrollment_for_installation(
                &second_enrollment,
                Some(installation_id),
                Some("Builder two"),
                None,
            )
            .await
            .expect("claim second enrollment");

        assert_eq!(first.server_id, second.server_id);
        let machine_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM machines WHERE workspace_id = $1 AND installation_id = $2",
        )
        .bind("workspace-a")
        .bind(installation_id)
        .fetch_one(&store.pool)
        .await
        .expect("count machines");
        assert_eq!(machine_count, 1);
        let stored_name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM machine_names WHERE workspace_id = $1 AND server_id = $2",
        )
        .bind("workspace-a")
        .bind(&second.server_id)
        .fetch_one(&store.pool)
        .await
        .expect("load machine name");
        assert_eq!(stored_name, "Builder two");

        let mut old_headers = HeaderMap::new();
        old_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", first.machine_token))
                .expect("old authorization"),
        );
        assert!(store.authenticate_machine(&old_headers).await.is_err());
        let mut new_headers = HeaderMap::new();
        new_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", second.machine_token))
                .expect("new authorization"),
        );
        assert!(store.authenticate_machine(&new_headers).await.is_ok());
    }

    #[tokio::test]
    async fn enrollment_recovers_a_revoked_legacy_machine_without_its_old_credential() {
        let store = AuthStore::for_test("owner-password").await;
        let first_enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create legacy enrollment");
        let legacy = store
            .claim_machine_enrollment(&first_enrollment)
            .await
            .expect("claim legacy enrollment");
        sqlx::query("UPDATE machines SET revoked_at = $1 WHERE server_id = $2")
            .bind(Utc::now().to_rfc3339())
            .bind(&legacy.server_id)
            .execute(&store.pool)
            .await
            .expect("revoke legacy machine");

        let installation_id = "mid_abcdef0123456789abcdef0123456789";
        let replacement_enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create replacement enrollment");
        let replacement = store
            .claim_machine_enrollment_for_installation(
                &replacement_enrollment,
                Some(installation_id),
                Some("Recovered builder"),
                Some(&legacy.server_id),
            )
            .await
            .expect("recover revoked machine");

        assert_eq!(replacement.server_id, legacy.server_id);
        let stored_installation = sqlx::query_scalar::<_, String>(
            "SELECT installation_id FROM machines WHERE server_id = $1",
        )
        .bind(&legacy.server_id)
        .fetch_one(&store.pool)
        .await
        .expect("load recovered installation identity");
        assert_eq!(stored_installation, installation_id);

        let mut old_headers = HeaderMap::new();
        old_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", legacy.machine_token))
                .expect("old authorization"),
        );
        assert!(store.authenticate_machine(&old_headers).await.is_err());
        let mut new_headers = HeaderMap::new();
        new_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", replacement.machine_token))
                .expect("new authorization"),
        );
        assert!(store.authenticate_machine(&new_headers).await.is_ok());
    }

    #[tokio::test]
    async fn existing_machine_can_bind_an_installation_identity_for_migration() {
        let store = AuthStore::for_test("owner-password").await;
        let first_enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create legacy enrollment");
        let legacy = store
            .claim_machine_enrollment(&first_enrollment)
            .await
            .expect("claim legacy enrollment");
        let installation_id = "mid_abcdef0123456789abcdef0123456789";
        store
            .bind_machine_identity(
                "workspace-a",
                &legacy.server_id,
                installation_id,
                "Migrated builder",
            )
            .await
            .expect("bind installation identity");
        let replacement = store
            .bind_machine_identity(
                "workspace-a",
                &legacy.server_id,
                "mid_11111111111111111111111111111111",
                "Other builder",
            )
            .await
            .expect_err("installation identity is immutable");
        assert_eq!(replacement.into_parts().1.code, "machine_identity_conflict");

        let second_enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create replacement enrollment");
        let reenrolled = store
            .claim_machine_enrollment_for_installation(
                &second_enrollment,
                Some(installation_id),
                Some("Migrated builder"),
                None,
            )
            .await
            .expect("reenroll migrated machine");
        assert_eq!(reenrolled.server_id, legacy.server_id);
    }

    #[tokio::test]
    async fn machine_authentication_rejects_missing_credentials() {
        let store = AuthStore::for_test("owner-password").await;
        assert!(store.authenticate_machine(&HeaderMap::new()).await.is_err());
    }

    #[tokio::test]
    async fn deleting_machine_revokes_credential_and_cleans_names() {
        let store = AuthStore::for_test("owner-password").await;
        let enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create enrollment");
        let claim = store
            .claim_machine_enrollment(&enrollment)
            .await
            .expect("claim enrollment");
        store
            .set_machine_name("workspace-a", &claim.server_id, "builder")
            .await
            .expect("store machine name");
        store
            .set_agent_name("workspace-a", "agent-a", "reviewer")
            .await
            .expect("store agent name");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", claim.machine_token))
                .expect("authorization header"),
        );
        assert!(store
            .machine_is_active("workspace-a", &claim.server_id)
            .await
            .expect("check active machine"));
        assert_eq!(
            store.active_machine_count().await.expect("machine count"),
            1
        );

        store
            .delete_machine("workspace-a", &claim.server_id, &["agent-a".to_string()])
            .await
            .expect("delete machine");

        assert!(store.authenticate_machine(&headers).await.is_err());
        assert!(!store
            .machine_is_active("workspace-a", &claim.server_id)
            .await
            .expect("check revoked machine"));
        assert_eq!(
            store.active_machine_count().await.expect("machine count"),
            0
        );
        let machine_name =
            sqlx::query_scalar::<_, String>("SELECT name FROM machine_names WHERE server_id = $1")
                .bind(&claim.server_id)
                .fetch_optional(&store.pool)
                .await
                .expect("query machine name");
        let agent_name =
            sqlx::query_scalar::<_, String>("SELECT name FROM agent_names WHERE agent_id = $1")
                .bind("agent-a")
                .fetch_optional(&store.pool)
                .await
                .expect("query agent name");
        assert!(machine_name.is_none());
        assert!(agent_name.is_none());
    }

    #[tokio::test]
    async fn persisted_names_are_applied_to_controller_snapshots() {
        let store = AuthStore::for_test("owner-password").await;
        store
            .set_machine_name("workspace-a", "server-a", "build-machine")
            .await
            .expect("store machine name");
        store
            .set_agent_name("workspace-a", "agent-a", "reviewer")
            .await
            .expect("store agent name");
        let now = Utc::now();
        let mut snapshot = AgentServerSnapshot {
            server: ServerInfo {
                server_id: "server-a".to_string(),
                workspace_id: "workspace-a".to_string(),
                name: String::new(),
                hostname: "original-host".to_string(),
                root: "/workspace".to_string(),
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
                status: ServerStatus::Online,
                connected_at: now,
                last_seen_at: now,
            },
            agents: vec![AgentInfo {
                agent_id: "agent-a".to_string(),
                workspace_id: "workspace-a".to_string(),
                server_id: "server-a".to_string(),
                kind: "codex".to_string(),
                name: "original-agent".to_string(),
                cwd: ".".to_string(),
                status: AgentStatus::Idle,
                pid: None,
                started_at: now,
                updated_at: now,
                exited_at: None,
                exit_code: None,
                output_revision: 0,
                interface: None,
            }],
        };

        store
            .apply_server_name(&mut snapshot.server)
            .await
            .expect("apply machine name");
        store
            .apply_agent_names(&mut snapshot)
            .await
            .expect("apply agent names");

        assert_eq!(snapshot.server.name, "build-machine");
        assert_eq!(snapshot.agents[0].name, "reviewer");

        store
            .delete_agent("workspace-a", "agent-a")
            .await
            .expect("persist deletion");
        snapshot.agents[0].name = "original-agent".to_string();
        let deleted = store
            .apply_agent_names(&mut snapshot)
            .await
            .expect("apply deletion");
        assert_eq!(deleted, ["agent-a"]);
        assert!(snapshot.agents.is_empty());
    }

    #[test]
    fn machine_route_is_limited_to_credential_workspace() {
        let machine = MachineSession {
            server_id: Some("srv_test".to_string()),
            workspace_id: Some("team one".to_string()),
        };
        assert!(machine_workspace_matches(
            &machine,
            "/agent/workspaces/team%20one/agents"
        ));
        assert!(!machine_workspace_matches(
            &machine,
            "/agent/workspaces/other/agents"
        ));
        assert!(machine_workspace_matches(
            &machine,
            "/agent/machine/identity"
        ));
    }
}
