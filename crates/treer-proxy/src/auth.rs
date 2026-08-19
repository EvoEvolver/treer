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
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Row, Transaction};
use subtle::ConstantTimeEq;
use treer_protocol::{
    format_machine_enrollment_key, parse_machine_enrollment_key, AgentInboxResponse,
    AgentMailMessage, AgentServerSnapshot, ApiError, CreateMachineServiceRequest,
    CreateVirtualNetworkHostRequest, MachineService, MachineServiceProtocol, MailAddress,
    MailAddressKind, MailDelivery, MailboxResponse, ProtocolError, ServerInfo,
    UpdateMachineServiceRequest, VirtualNetworkHost, WorkspaceHuman, WorkspaceInfo,
    AGENT_ID_HEADER, WORKLOAD_CREDENTIAL_HEADER,
};
use url::Url;
use uuid::Uuid;

use crate::state::AppState;

const SESSION_COOKIE: &str = "treer_session";
const ADMIN_SESSION_COOKIE: &str = "treer_admin_session";
const SESSION_TTL_DAYS: i64 = 30;
const ADMIN_SESSION_TTL_HOURS: i64 = 8;
const PASSWORD_RESET_TTL_MINUTES: i64 = 30;
const PASSWORD_RESET_RATE_LIMIT_SECONDS: i64 = 60;
const OAUTH_STATE_TTL_MINUTES: i64 = 10;
const MACHINE_ENROLLMENT_TTL_MINUTES: i64 = 10;
const AGENT_CREDENTIAL_CACHE_TTL: StdDuration = StdDuration::from_secs(5);
const MAX_MAIL_BODY_BYTES: usize = 32 * 1024;
const MAX_MAIL_RECIPIENTS: usize = 32;
const MAX_MAIL_CONTEXTS: usize = 32;
const MAX_INBOX_LIMIT: u16 = 100;

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

struct PendingPasswordReset {
    token_id: String,
    recipient: String,
    url: Url,
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
        };
        store.initialize_schema().await?;
        store.refresh_virtual_network_hosts().await?;
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
            "SELECT workspace_id, name, created_at FROM workspaces ORDER BY workspace_id",
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
        let result = sqlx::query("UPDATE organizations SET name = $1 WHERE organization_id = $2")
            .bind(&name)
            .bind(organization_id)
            .execute(&self.pool)
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
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
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
        let organization_id = sqlx::query_scalar::<_, String>(
            "SELECT organization_id FROM workspaces WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        self.require_organization_member(&organization_id, user_id)
            .await?;
        Ok(())
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
             WHERE w.workspace_id = $1 \
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
             WHERE organization_id = $1 ORDER BY lower(name), workspace_id",
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
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(workspace_id)
        .bind(organization_id)
        .bind(&name)
        .bind(now.to_rfc3339())
        .bind(user_id)
        .execute(&self.pool)
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
        Ok(WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            name,
            created_at: now,
        })
    }

    pub async fn list_machine_services(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<MachineService>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT service_id, workspace_id, name, server_id, target_host, target_port, \
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
            "SELECT service_id, workspace_id, name, server_id, target_host, target_port, \
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
                "SELECT service_id, workspace_id, name, server_id, target_host, target_port, \
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
        let target_host = validate_service_target_host(&request.target_host)?;
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
             service_id, workspace_id, name, server_id, target_host, target_port, protocol, \
             created_at, created_by, updated_at, updated_by) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&service.service_id)
        .bind(&service.workspace_id)
        .bind(&service.name)
        .bind(&service.server_id)
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
             s.server_id AS destination_server_id, s.target_host, s.target_port, v.created_at, v.created_by \
             FROM virtual_network_hosts v JOIN machine_services s ON s.service_id = v.service_id",
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

    pub async fn send_agent_mail(
        &self,
        workspace_id: &str,
        sender: MailAddress,
        recipients: Vec<MailAddress>,
        context_ids: Vec<String>,
        body: &str,
    ) -> Result<AgentMailMessage, AuthFailure> {
        if body.trim().is_empty() || body.len() > MAX_MAIL_BODY_BYTES {
            return Err(AuthFailure::bad_request(
                "invalid_mail_body",
                "mail body must contain 1-32768 bytes",
            ));
        }
        if recipients.is_empty() || recipients.len() > MAX_MAIL_RECIPIENTS {
            return Err(AuthFailure::bad_request(
                "invalid_mail_recipients",
                "mail must have 1-32 unique recipients",
            ));
        }
        if context_ids.len() > MAX_MAIL_CONTEXTS {
            return Err(AuthFailure::bad_request(
                "invalid_mail_context",
                "mail may reference at most 32 context messages",
            ));
        }

        let message_id = format!("msg_{}", Uuid::new_v4().simple());
        let created_at = Utc::now();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        for context_id in &context_ids {
            let accessible = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM mail_messages m \
                 WHERE m.message_id = $1 AND m.workspace_id = $2 \
                 AND ((m.sender_kind = 'agent' AND m.sender_id = $3) OR EXISTS(\
                     SELECT 1 FROM mail_recipients r \
                     WHERE r.message_id = m.message_id \
                       AND r.recipient_kind = 'agent' AND r.recipient_id = $3)))",
            )
            .bind(context_id)
            .bind(workspace_id)
            .bind(&sender.id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            if !accessible {
                return Err(AuthFailure::bad_request(
                    "invalid_mail_context",
                    "context message does not exist or is not visible to the sender",
                ));
            }
        }

        sqlx::query(
            "INSERT INTO mail_messages(\
                message_id, workspace_id, sender_kind, sender_id, sender_name, body, created_at\
             ) VALUES($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&message_id)
        .bind(workspace_id)
        .bind(match sender.kind {
            MailAddressKind::Agent => "agent",
            MailAddressKind::Human => "human",
        })
        .bind(&sender.id)
        .bind(&sender.name)
        .bind(body)
        .bind(created_at.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        for (position, recipient) in recipients.iter().enumerate() {
            sqlx::query(
                "INSERT INTO mail_recipients(\
                    message_id, workspace_id, recipient_kind, recipient_id, recipient_name, \
                    position, created_at\
                 ) VALUES($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&message_id)
            .bind(workspace_id)
            .bind(match recipient.kind {
                MailAddressKind::Agent => "agent",
                MailAddressKind::Human => "human",
            })
            .bind(&recipient.id)
            .bind(&recipient.name)
            .bind(position as i64)
            .bind(created_at.to_rfc3339())
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        for (position, context_id) in context_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO mail_contexts(message_id, context_message_id, position) \
                 VALUES($1, $2, $3)",
            )
            .bind(&message_id)
            .bind(context_id)
            .bind(position as i64)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                if error
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation())
                {
                    AuthFailure::bad_request(
                        "invalid_mail_context",
                        "mail context messages must be unique",
                    )
                } else {
                    AuthFailure::database(error)
                }
            })?;
        }
        transaction.commit().await.map_err(AuthFailure::database)?;

        Ok(AgentMailMessage {
            message_id,
            workspace_id: workspace_id.to_string(),
            sender,
            recipients,
            context_ids,
            body: body.to_string(),
            created_at,
        })
    }

    pub async fn read_agent_inbox(
        &self,
        workspace_id: &str,
        recipient_agent_id: &str,
        limit: u16,
    ) -> Result<AgentInboxResponse, AuthFailure> {
        if limit == 0 || limit > MAX_INBOX_LIMIT {
            return Err(AuthFailure::bad_request(
                "invalid_inbox_limit",
                "inbox limit must be between 1 and 100",
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let rows = sqlx::query(
            "SELECT m.message_id, m.workspace_id, m.sender_kind, m.sender_id, m.sender_name, \
                    m.body, m.created_at \
             FROM mail_recipients r \
             JOIN mail_messages m ON m.message_id = r.message_id \
             WHERE r.workspace_id = $1 AND r.recipient_kind = 'agent' \
               AND r.recipient_id = $2 AND r.read_at IS NULL \
             ORDER BY r.created_at, r.message_id LIMIT $3 \
             FOR UPDATE OF r SKIP LOCKED",
        )
        .bind(workspace_id)
        .bind(recipient_agent_id)
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let message_ids: Vec<String> = rows.iter().map(|row| row.get("message_id")).collect();
        let mut recipients_by_message = HashMap::<String, Vec<MailAddress>>::new();
        let mut contexts_by_message = HashMap::<String, Vec<String>>::new();
        if !message_ids.is_empty() {
            let recipient_rows = sqlx::query(
                "SELECT message_id, recipient_kind, recipient_id, recipient_name \
                 FROM mail_recipients WHERE message_id = ANY($1) \
                 ORDER BY message_id, position",
            )
            .bind(&message_ids)
            .fetch_all(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            for row in recipient_rows {
                recipients_by_message
                    .entry(row.get("message_id"))
                    .or_default()
                    .push(MailAddress {
                        kind: if row.get::<String, _>("recipient_kind") == "agent" {
                            MailAddressKind::Agent
                        } else {
                            MailAddressKind::Human
                        },
                        id: row.get("recipient_id"),
                        name: row.get("recipient_name"),
                    });
            }
            let context_rows = sqlx::query(
                "SELECT message_id, context_message_id FROM mail_contexts \
                 WHERE message_id = ANY($1) ORDER BY message_id, position",
            )
            .bind(&message_ids)
            .fetch_all(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            for row in context_rows {
                contexts_by_message
                    .entry(row.get("message_id"))
                    .or_default()
                    .push(row.get("context_message_id"));
            }
        }

        let mut messages = Vec::with_capacity(rows.len());
        for row in rows {
            let message_id: String = row.get("message_id");
            messages.push(AgentMailMessage {
                message_id: message_id.clone(),
                workspace_id: row.get("workspace_id"),
                sender: MailAddress {
                    kind: if row.get::<String, _>("sender_kind") == "agent" {
                        MailAddressKind::Agent
                    } else {
                        MailAddressKind::Human
                    },
                    id: row.get("sender_id"),
                    name: row.get("sender_name"),
                },
                recipients: recipients_by_message
                    .remove(&message_id)
                    .unwrap_or_default(),
                context_ids: contexts_by_message.remove(&message_id).unwrap_or_default(),
                body: row.get("body"),
                created_at: parse_database_timestamp(&row, "created_at", "agent message")?,
            });
        }
        if !message_ids.is_empty() {
            sqlx::query(
                "UPDATE mail_recipients SET read_at = $1 \
                 WHERE workspace_id = $2 AND recipient_kind = 'agent' AND recipient_id = $3 \
                   AND message_id = ANY($4) AND read_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(workspace_id)
            .bind(recipient_agent_id)
            .bind(&message_ids)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        let remaining = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mail_recipients \
             WHERE workspace_id = $1 AND recipient_kind = 'agent' \
               AND recipient_id = $2 AND read_at IS NULL",
        )
        .bind(workspace_id)
        .bind(recipient_agent_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(AgentInboxResponse {
            messages,
            remaining_unread: u64::try_from(remaining).unwrap_or(0),
        })
    }

    pub async fn read_human_mailbox(
        &self,
        workspace_id: &str,
        recipient_user_id: &str,
        limit: u16,
    ) -> Result<MailboxResponse, AuthFailure> {
        if limit == 0 || limit > MAX_INBOX_LIMIT {
            return Err(AuthFailure::bad_request(
                "invalid_inbox_limit",
                "inbox limit must be between 1 and 100",
            ));
        }
        self.require_workspace_member(workspace_id, recipient_user_id)
            .await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let rows = sqlx::query(
            "SELECT m.message_id, m.workspace_id, m.sender_kind, m.sender_id, m.sender_name, \
                    m.body, m.created_at, r.read_at \
             FROM mail_recipients r \
             JOIN mail_messages m ON m.message_id = r.message_id \
             WHERE r.workspace_id = $1 AND r.recipient_kind = 'human' \
               AND r.recipient_id = $2 \
             ORDER BY r.created_at DESC, r.message_id DESC LIMIT $3 \
             FOR UPDATE OF r",
        )
        .bind(workspace_id)
        .bind(recipient_user_id)
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let message_ids: Vec<String> = rows.iter().map(|row| row.get("message_id")).collect();
        let mut recipients_by_message = HashMap::<String, Vec<MailAddress>>::new();
        let mut contexts_by_message = HashMap::<String, Vec<String>>::new();
        if !message_ids.is_empty() {
            let recipient_rows = sqlx::query(
                "SELECT message_id, recipient_kind, recipient_id, recipient_name \
                 FROM mail_recipients WHERE message_id = ANY($1) \
                 ORDER BY message_id, position",
            )
            .bind(&message_ids)
            .fetch_all(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            for row in recipient_rows {
                recipients_by_message
                    .entry(row.get("message_id"))
                    .or_default()
                    .push(MailAddress {
                        kind: if row.get::<String, _>("recipient_kind") == "agent" {
                            MailAddressKind::Agent
                        } else {
                            MailAddressKind::Human
                        },
                        id: row.get("recipient_id"),
                        name: row.get("recipient_name"),
                    });
            }
            let context_rows = sqlx::query(
                "SELECT message_id, context_message_id FROM mail_contexts \
                 WHERE message_id = ANY($1) ORDER BY message_id, position",
            )
            .bind(&message_ids)
            .fetch_all(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            for row in context_rows {
                contexts_by_message
                    .entry(row.get("message_id"))
                    .or_default()
                    .push(row.get("context_message_id"));
            }
        }

        let mut deliveries = Vec::with_capacity(rows.len());
        for row in rows {
            let message_id: String = row.get("message_id");
            let unread = row.get::<Option<String>, _>("read_at").is_none();
            deliveries.push(MailDelivery {
                unread,
                message: AgentMailMessage {
                    message_id: message_id.clone(),
                    workspace_id: row.get("workspace_id"),
                    sender: MailAddress {
                        kind: if row.get::<String, _>("sender_kind") == "agent" {
                            MailAddressKind::Agent
                        } else {
                            MailAddressKind::Human
                        },
                        id: row.get("sender_id"),
                        name: row.get("sender_name"),
                    },
                    recipients: recipients_by_message
                        .remove(&message_id)
                        .unwrap_or_default(),
                    context_ids: contexts_by_message.remove(&message_id).unwrap_or_default(),
                    body: row.get("body"),
                    created_at: parse_database_timestamp(&row, "created_at", "agent message")?,
                },
            });
        }
        if !message_ids.is_empty() {
            sqlx::query(
                "UPDATE mail_recipients SET read_at = $1 \
                 WHERE workspace_id = $2 AND recipient_kind = 'human' AND recipient_id = $3 \
                   AND message_id = ANY($4) AND read_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(workspace_id)
            .bind(recipient_user_id)
            .bind(&message_ids)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        let remaining = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mail_recipients \
             WHERE workspace_id = $1 AND recipient_kind = 'human' \
               AND recipient_id = $2 AND read_at IS NULL",
        )
        .bind(workspace_id)
        .bind(recipient_user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(MailboxResponse {
            deliveries,
            remaining_unread: u64::try_from(remaining).unwrap_or(0),
        })
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
        self.claim_machine_enrollment_for_installation(token, None, None)
            .await
    }

    pub async fn claim_machine_enrollment_for_installation(
        &self,
        token: &str,
        installation_id: Option<&str>,
        machine_name: Option<&str>,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        let installation_id = installation_id.map(validate_installation_id).transpose()?;
        let machine_name = machine_name
            .map(|name| validate_resource_name(name, "machine"))
            .transpose()?;
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
        let existing_server_id = if let Some(installation_id) = installation_id.as_deref() {
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
        let server_id = existing_server_id
            .clone()
            .unwrap_or_else(|| format!("srv_{}", Uuid::new_v4().simple()));
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
        if existing_server_id.is_some() {
            sqlx::query(
                "UPDATE machines SET secret_hash = $1, enrolled_by = $2, revoked_at = NULL \
                 WHERE server_id = $3 AND workspace_id = $4",
            )
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
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        let token = bearer_token(headers).ok_or_else(invalid_machine_enrollment)?;
        self.claim_machine_enrollment_for_installation(token, installation_id, machine_name)
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
            "SELECT workspace_id, secret_hash FROM machines \
             WHERE server_id = $1 AND revoked_at IS NULL",
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
                "SELECT workspace_id, server_id, secret_hash FROM agent_credentials \
                 WHERE agent_id = $1 AND revoked_at IS NULL",
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

    async fn create_password_reset(
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
        sqlx::query(
            "INSERT INTO invitations(\
             token, created_at, created_by, kind, organization_id, role) \
             VALUES($1, $2, $3, 'organization', $4, 'member')",
        )
        .bind(&token)
        .bind(Utc::now().to_rfc3339())
        .bind(created_by)
        .bind(organization_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let mut url = self.app_public_url.clone();
        url.set_path("/");
        url.query_pairs_mut().clear().append_pair("invite", &token);
        Ok((token, url))
    }

    async fn create_personal_invitation(&self) -> Result<(String, Url), AuthFailure> {
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
        let result = sqlx::query(
            "UPDATE organization_members SET role = $1 \
             WHERE organization_id = $2 AND user_id = $3 AND role != 'owner'",
        )
        .bind(role)
        .bind(organization_id)
        .bind(target_user_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "member_not_found",
                "member does not exist or is the organization owner",
            ));
        }
        Ok(())
    }

    pub async fn remove_member(
        &self,
        organization_id: &str,
        actor: &str,
        target_user_id: &str,
    ) -> Result<(), AuthFailure> {
        self.require_manager(organization_id, actor).await?;
        let target_role = self
            .membership_role(organization_id, target_user_id)
            .await?
            .ok_or_else(|| AuthFailure::not_found("member_not_found", "member does not exist"))?;
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
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
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

async fn authenticate_request(
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

pub async fn admin_dashboard(
    Extension(auth): Extension<AuthStore>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "machine_count": auth.active_machine_count().await?,
        "agent_count": state.platform_agent_count().await,
    })))
}

pub async fn admin_create_invitation(
    Extension(auth): Extension<AuthStore>,
) -> Result<Json<Value>, AuthFailure> {
    let (token, url) = auth.create_personal_invitation().await?;
    Ok(Json(json!({ "token": token, "url": url.as_str() })))
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

const fn machine_service_protocol_str(protocol: MachineServiceProtocol) -> &'static str {
    match protocol {
        MachineServiceProtocol::Tcp => "tcp",
        MachineServiceProtocol::Http => "http",
    }
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
    fn not_found(code: &str, message: &str) -> Self {
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

    fn internal(code: &str, message: String) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
    }

    fn database(error: sqlx::Error) -> Self {
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

    fn mail_address(agent_id: &str, name: &str) -> MailAddress {
        MailAddress {
            kind: MailAddressKind::Agent,
            id: agent_id.to_string(),
            name: name.to_string(),
        }
    }

    #[tokio::test]
    async fn workspace_humans_are_discoverable_and_receive_independent_mail() {
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
                "human-mail",
                "Human mail",
                &owner.user_id,
            )
            .await
            .expect("create workspace");
        let humans = store
            .list_workspace_humans("human-mail")
            .await
            .expect("list workspace humans");
        assert_eq!(
            humans,
            [WorkspaceHuman {
                user_id: owner.user_id.clone(),
                preferred_name: "Owner".to_string(),
                role: "owner".to_string(),
            }]
        );
        let human = MailAddress {
            kind: MailAddressKind::Human,
            id: owner.user_id.clone(),
            name: owner.preferred_name.clone(),
        };
        let agent = mail_address("agent-b", "reviewer");
        let sent = store
            .send_agent_mail(
                "human-mail",
                mail_address("agent-a", "builder"),
                vec![agent.clone(), human.clone()],
                vec![],
                "Deployment is ready.",
            )
            .await
            .expect("send human mail");
        assert_eq!(sent.recipients, [agent.clone(), human]);

        let mailbox = store
            .read_human_mailbox("human-mail", &owner.user_id, 50)
            .await
            .expect("read human mailbox");
        assert_eq!(mailbox.deliveries.len(), 1);
        assert!(mailbox.deliveries[0].unread);
        assert_eq!(mailbox.deliveries[0].message, sent);
        let agent_inbox = store
            .read_agent_inbox("human-mail", &agent.id, 50)
            .await
            .expect("read agent inbox independently");
        assert_eq!(agent_inbox.messages, [sent]);
        let reread = store
            .read_human_mailbox("human-mail", &owner.user_id, 50)
            .await
            .expect("reread human mailbox");
        assert_eq!(reread.deliveries.len(), 1);
        assert!(!reread.deliveries[0].unread);

        let outsider = bootstrap_owner(&store, "outsider@example.com", "Outsider").await;
        let error = store
            .read_human_mailbox("human-mail", &outsider.user_id, 50)
            .await
            .expect_err("non-member cannot read human inbox");
        assert_eq!(error.into_parts().1.code, "organization_access_denied");
    }

    #[tokio::test]
    async fn agent_mail_is_durable_scoped_threaded_and_marked_read_per_recipient() {
        let store = AuthStore::for_test("owner-password").await;
        store.seed_test_workspace("default").await;
        let alice = mail_address("agent-alice", "alice");
        let bob = mail_address("agent-bob", "bob");
        let charlie = mail_address("agent-charlie", "charlie");
        let root = store
            .send_agent_mail(
                "default",
                alice.clone(),
                vec![bob.clone(), charlie.clone()],
                vec![],
                "Please review the parser.",
            )
            .await
            .expect("send root message");
        assert!(root.message_id.starts_with("msg_"));
        assert_eq!(root.recipients, [bob.clone(), charlie.clone()]);

        let bob_inbox = store
            .read_agent_inbox("default", &bob.id, 100)
            .await
            .expect("read bob inbox");
        assert_eq!(bob_inbox.messages, std::slice::from_ref(&root));
        assert_eq!(bob_inbox.remaining_unread, 0);
        assert!(store
            .read_agent_inbox("default", &bob.id, 100)
            .await
            .expect("reread bob inbox")
            .messages
            .is_empty());

        let charlie_inbox = store
            .read_agent_inbox("default", &charlie.id, 100)
            .await
            .expect("read charlie inbox");
        assert_eq!(charlie_inbox.messages, std::slice::from_ref(&root));

        let reply = store
            .send_agent_mail(
                "default",
                bob.clone(),
                vec![alice.clone()],
                vec![root.message_id.clone()],
                "Review complete.",
            )
            .await
            .expect("send threaded reply");
        let alice_inbox = store
            .read_agent_inbox("default", &alice.id, 100)
            .await
            .expect("read alice inbox");
        assert_eq!(alice_inbox.messages, [reply]);

        let error = store
            .send_agent_mail(
                "default",
                mail_address("agent-outsider", "outsider"),
                vec![alice],
                vec![root.message_id],
                "Forge a reply.",
            )
            .await
            .expect_err("unrelated sender cannot reference context");
        assert_eq!(error.into_parts().1.code, "invalid_mail_context");
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
                labels: Default::default(),
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
