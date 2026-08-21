mod model;
mod store;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{Duration, Utc};
use clap::Parser;
use model::{HumanSession, InboxRequest, PendingOAuth, SendMessageRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use store::MailStore;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use treer_protocol::{
    AppIdentityClaims, AppIdentityTokenResponse, AppIdentityVerifyRequest,
    AppIdentityVerifyResponse, AppPrincipal, AppPrincipalKind, ResolveAppRecipientsRequest,
    ResolveAppRecipientsResponse,
};
use url::Url;
use uuid::Uuid;

const SESSION_COOKIE: &str = "treer_mail_session";
const OAUTH_STATE_TTL_MINUTES: i64 = 10;

#[derive(Debug, Parser)]
#[command(name = "treer-mail", about = "Optional Treer workspace mail app")]
struct Args {
    #[arg(long, env = "TREER_MAIL_LISTEN", default_value = "127.0.0.1:8788")]
    listen: SocketAddr,
    #[arg(
        long,
        env = "TREER_MAIL_DATABASE_URL",
        default_value = "sqlite://treer-mail.db?mode=rwc",
        hide_env_values = true
    )]
    database_url: String,
    #[arg(long, env = "TREER_PROXY_PUBLIC_URL")]
    proxy_public_url: Url,
    #[arg(long, env = "TREER_MAIL_SERVICE_ID")]
    service_id: String,
    #[arg(long, env = "TREER_MAIL_PUBLIC_URL")]
    public_url: Url,
    #[arg(long, env = "TREER_MAIL_WEB_DIR", default_value = "apps/mail/web/dist")]
    web_dir: PathBuf,
}

#[derive(Clone)]
struct AppState {
    store: MailStore,
    client: reqwest::Client,
    proxy_public_url: Url,
    service_id: String,
    public_url: Url,
    secure_cookie: bool,
}

#[derive(Debug)]
struct RequestIdentity {
    principal: AppPrincipal,
    workspace_id: String,
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct ReturnToQuery {
    #[serde(default = "default_return_path")]
    return_to: String,
}

#[derive(Debug, Deserialize)]
struct OAuthCallbackQuery {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct MailboxQuery {
    #[serde(default = "default_mailbox_limit")]
    limit: u16,
}

#[derive(Debug, Serialize)]
struct SessionResponse {
    workspace_id: String,
    service_id: String,
    user: AppPrincipal,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "treer_mail=info,tower_http=info".into()),
        )
        .init();
    let args = Args::parse();
    validate_config(&args)?;
    let store = MailStore::open(&args.database_url).await?;
    let index = args.web_dir.join("index.html");
    let state = AppState {
        store,
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .context("build Proxy client")?,
        proxy_public_url: normalized_base_url(args.proxy_public_url),
        service_id: args.service_id,
        secure_cookie: args.public_url.scheme() == "https",
        public_url: normalized_base_url(args.public_url),
    };
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(config))
        .route("/api/auth/start", get(start_oauth))
        .route("/api/auth/callback", get(oauth_callback))
        .route("/api/auth/session", get(session))
        .route("/api/auth/logout", post(logout))
        .route("/api/directory", get(directory))
        .route("/api/messages", get(recent_messages).post(send_message))
        .route("/api/inbox", post(unread_inbox))
        .fallback_service(ServeDir::new(args.web_dir).fallback(ServeFile::new(index)))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind mail server at {}", args.listen))?;
    tracing::info!(address = %args.listen, "Treer Mail listening");
    axum::serve(listener, app).await.context("serve Treer Mail")
}

fn validate_config(args: &Args) -> anyhow::Result<()> {
    if args.service_id.trim().is_empty() || args.service_id.len() > 128 {
        anyhow::bail!("TREER_MAIL_SERVICE_ID must be a registered service ID");
    }
    if !matches!(args.proxy_public_url.scheme(), "http" | "https") {
        anyhow::bail!("TREER_PROXY_PUBLIC_URL must use HTTP or HTTPS");
    }
    if !matches!(args.public_url.scheme(), "http" | "https") || args.public_url.host().is_none() {
        anyhow::bail!("TREER_MAIL_PUBLIC_URL must be an absolute HTTP or HTTPS URL");
    }
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "treer-mail" }))
}

async fn config(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "service_id": state.service_id,
        "proxy_public_url": state.proxy_public_url.as_str(),
    }))
}

async fn start_oauth(
    State(state): State<AppState>,
    Query(query): Query<ReturnToQuery>,
) -> Result<Response, AppError> {
    let return_path = local_return_path(&query.return_to)?;
    let verifier = random_token();
    let oauth_state = random_token();
    let pending = PendingOAuth {
        state_hash: secret_hash(&oauth_state),
        verifier: verifier.clone(),
        return_path,
        expires_at: Utc::now() + Duration::minutes(OAUTH_STATE_TTL_MINUTES),
    };
    state.store.save_oauth_state(&pending).await?;
    let mut authorize = state.proxy_public_url.join("api/apps/oauth/authorize")?;
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &state.service_id)
        .append_pair("redirect_uri", oauth_callback_url(&state)?.as_str())
        .append_pair("state", &oauth_state)
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256");
    Ok(Redirect::to(authorize.as_str()).into_response())
}

async fn oauth_callback(
    State(state): State<AppState>,
    Query(query): Query<OAuthCallbackQuery>,
) -> Result<Response, AppError> {
    let pending = state
        .store
        .consume_oauth_state(&secret_hash(&query.state))
        .await?
        .ok_or_else(|| AppError::unauthorized("OAuth state is invalid or expired"))?;
    let token_url = state.proxy_public_url.join("api/apps/oauth/token")?;
    let callback_url = oauth_callback_url(&state)?;
    let response = state
        .client
        .post(token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", query.code.as_str()),
            ("client_id", state.service_id.as_str()),
            ("redirect_uri", callback_url.as_str()),
            ("code_verifier", pending.verifier.as_str()),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(AppError::unauthorized("Proxy rejected the OAuth code"));
    }
    let token = response.json::<AppIdentityTokenResponse>().await?;
    let claims = verify_token(&state, &token.access_token).await?;
    if claims.principal_kind != AppPrincipalKind::Human || claims.service_id != state.service_id {
        return Err(AppError::unauthorized(
            "Proxy returned an identity for the wrong principal or service",
        ));
    }
    let raw_session = format!("mas_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    state
        .store
        .save_session(&HumanSession {
            token_hash: secret_hash(&raw_session),
            access_token: token.access_token,
            workspace_id: claims.workspace_id,
            service_id: claims.service_id,
            user_id: claims.sub,
            preferred_name: claims.name,
            role: claims.role.unwrap_or_else(|| "member".to_string()),
            expires_at: token.expires_at,
        })
        .await?;
    let cookie = session_cookie(&raw_session, state.secure_cookie, token.expires_in);
    let mut redirect = Redirect::to(&pending.return_path).into_response();
    redirect.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| AppError::internal("invalid session cookie"))?,
    );
    Ok(redirect)
}

async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, AppError> {
    let session = human_session(&state, &headers)
        .await?
        .ok_or_else(|| AppError::unauthorized("Mail login required"))?;
    Ok(Json(SessionResponse {
        workspace_id: session.workspace_id,
        service_id: session.service_id,
        user: AppPrincipal {
            kind: AppPrincipalKind::Human,
            id: session.user_id,
            name: session.preferred_name,
            role: Some(session.role),
        },
    }))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    if let Some(raw) = cookie_value(&headers, SESSION_COOKIE) {
        state.store.delete_session(&secret_hash(&raw)).await?;
    }
    let cookie = format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if state.secure_cookie { "; Secure" } else { "" }
    );
    Ok(([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response())
}

async fn directory(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let identity = request_identity(&state, &headers).await?;
    let url = state.proxy_public_url.join(&format!(
        "api/apps/{}/directory",
        url_encode(&state.service_id)
    ))?;
    let response = state
        .client
        .get(url)
        .bearer_auth(&identity.access_token)
        .send()
        .await?;
    proxy_json(response).await
}

async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SendMessageRequest>,
) -> Result<Json<Value>, AppError> {
    let identity = request_identity(&state, &headers).await?;
    if request.recipients.is_empty() || request.recipients.len() > 32 {
        return Err(AppError::bad_request("message requires 1-32 recipients"));
    }
    let url = state.proxy_public_url.join(&format!(
        "api/apps/{}/recipients/resolve",
        url_encode(&state.service_id)
    ))?;
    let response = state
        .client
        .post(url)
        .bearer_auth(&identity.access_token)
        .json(&ResolveAppRecipientsRequest {
            recipients: request.recipients,
        })
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(proxy_error(response).await);
    }
    let resolved = response.json::<ResolveAppRecipientsResponse>().await?;
    if resolved.sender.kind != identity.principal.kind
        || resolved.sender.id != identity.principal.id
    {
        return Err(AppError::unauthorized(
            "Proxy resolved a sender that does not match the authenticated principal",
        ));
    }
    let message = state
        .store
        .send_message(
            &identity.workspace_id,
            &resolved.sender,
            &resolved.recipients,
            &request.context_ids,
            &request.body,
        )
        .await
        .map_err(|error| AppError::bad_request(&error.to_string()))?;
    Ok(Json(json!({ "message": message })))
}

async fn unread_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InboxRequest>,
) -> Result<Json<model::MailboxResponse>, AppError> {
    let identity = request_identity(&state, &headers).await?;
    Ok(Json(
        state
            .store
            .unread_inbox(&identity.workspace_id, &identity.principal, request.limit)
            .await
            .map_err(|error| AppError::bad_request(&error.to_string()))?,
    ))
}

async fn recent_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<MailboxQuery>,
) -> Result<Json<model::MailboxResponse>, AppError> {
    let identity = request_identity(&state, &headers).await?;
    Ok(Json(
        state
            .store
            .recent_mailbox(&identity.workspace_id, &identity.principal, query.limit)
            .await
            .map_err(|error| AppError::bad_request(&error.to_string()))?,
    ))
}

async fn request_identity(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<RequestIdentity, AppError> {
    if let Some(session) = human_session(state, headers).await? {
        return Ok(RequestIdentity {
            principal: AppPrincipal {
                kind: AppPrincipalKind::Human,
                id: session.user_id,
                name: session.preferred_name,
                role: Some(session.role),
            },
            workspace_id: session.workspace_id,
            access_token: session.access_token,
        });
    }
    let token = bearer_token(headers)
        .ok_or_else(|| AppError::unauthorized("Treer identity token required"))?;
    let claims = verify_token(state, token).await?;
    Ok(RequestIdentity {
        principal: AppPrincipal {
            kind: claims.principal_kind,
            id: claims.sub,
            name: claims.name,
            role: claims.role,
        },
        workspace_id: claims.workspace_id,
        access_token: token.to_string(),
    })
}

async fn human_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<HumanSession>, AppError> {
    let Some(raw) = cookie_value(headers, SESSION_COOKIE) else {
        return Ok(None);
    };
    state
        .store
        .session(&secret_hash(&raw))
        .await
        .map_err(Into::into)
}

async fn verify_token(state: &AppState, token: &str) -> Result<AppIdentityClaims, AppError> {
    let url = state.proxy_public_url.join(".treer/apps/identity/verify")?;
    let response = state
        .client
        .post(url)
        .json(&AppIdentityVerifyRequest {
            token: token.to_string(),
            audience: state.service_id.clone(),
        })
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(AppError::unauthorized("Proxy identity verification failed"));
    }
    response
        .json::<AppIdentityVerifyResponse>()
        .await?
        .claims
        .ok_or_else(|| AppError::unauthorized("Treer identity token is inactive"))
}

async fn proxy_json(response: reqwest::Response) -> Result<Json<Value>, AppError> {
    if !response.status().is_success() {
        return Err(proxy_error(response).await);
    }
    Ok(Json(response.json().await?))
}

async fn proxy_error(response: reqwest::Response) -> AppError {
    let status = response.status();
    let message = response
        .json::<Value>()
        .await
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Treer Proxy rejected the request".to_string());
    AppError { status, message }
}

fn oauth_callback_url(state: &AppState) -> Result<Url, AppError> {
    state
        .public_url
        .join("api/auth/callback")
        .map_err(Into::into)
}

fn session_cookie(raw: &str, secure: bool, max_age: u64) -> String {
    format!(
        "{SESSION_COOKIE}={raw}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    )
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
        .filter(|value| !value.is_empty())
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn secret_hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn local_return_path(value: &str) -> Result<String, AppError> {
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.len() > 4096
        || value.chars().any(char::is_control)
    {
        return Err(AppError::bad_request(
            "return_to must be a local absolute path",
        ));
    }
    Ok(value.to_string())
}

fn default_return_path() -> String {
    "/".to_string()
}

const fn default_mailbox_limit() -> u16 {
    100
}

fn normalized_base_url(mut url: Url) -> Url {
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url
}

fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }

    fn unauthorized(message: &str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.to_string(),
        }
    }

    fn internal(message: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "message": self.message } })),
        )
            .into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(%error, "mail app error");
        Self::internal("mail service operation failed")
    }
}

impl From<reqwest::Error> for AppError {
    fn from(error: reqwest::Error) -> Self {
        tracing::warn!(%error, "mail app Proxy request failed");
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: "Treer Proxy is unavailable".to_string(),
        }
    }
}

impl From<url::ParseError> for AppError {
    fn from(_: url::ParseError) -> Self {
        Self::internal("mail service URL configuration is invalid")
    }
}
