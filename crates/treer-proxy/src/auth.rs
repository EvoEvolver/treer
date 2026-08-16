use std::path::Path;
use std::sync::Arc;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Extension, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use treer_protocol::{ApiError, ProtocolError};
use url::Url;
use uuid::Uuid;

const SESSION_COOKIE: &str = "treer_session";
const SESSION_TTL_DAYS: i64 = 30;

#[derive(Clone)]
pub struct AuthStore {
    pool: SqlitePool,
    admin_password: Arc<str>,
    public_url: Url,
    disabled: bool,
}

#[derive(Clone, Debug)]
pub struct CurrentSession {
    pub token: String,
    pub username: String,
    pub is_admin: bool,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    invite: String,
    username: String,
    password: String,
}

impl AuthStore {
    pub async fn open(
        path: &Path,
        admin_password: String,
        public_url: Url,
        disabled: bool,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let store = Self {
            pool,
            admin_password: admin_password.into(),
            public_url,
            disabled,
        };
        store.migrate().await?;
        Ok(store)
    }

    #[cfg(test)]
    async fn in_memory(admin_password: &str) -> Self {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database");
        let store = Self {
            pool,
            admin_password: admin_password.to_string().into(),
            public_url: Url::parse("https://treer.example/").expect("valid URL"),
            disabled: false,
        };
        store.migrate().await.expect("database migration");
        store
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS users (\
             id TEXT PRIMARY KEY, \
             username TEXT NOT NULL COLLATE NOCASE UNIQUE, \
             password_hash TEXT NOT NULL, \
             created_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS invitations (\
             token TEXT PRIMARY KEY, \
             created_at TEXT NOT NULL, \
             created_by TEXT NOT NULL, \
             used_at TEXT, \
             used_by TEXT)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sessions (\
             token TEXT PRIMARY KEY, \
             username TEXT NOT NULL, \
             is_admin INTEGER NOT NULL, \
             created_at TEXT NOT NULL, \
             expires_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS sessions_expires_at ON sessions(expires_at)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn login(&self, username: &str, password: &str) -> Result<CurrentSession, AuthFailure> {
        let username = username.trim();
        let is_admin = username.eq_ignore_ascii_case("admin");
        let valid = if is_admin {
            password == self.admin_password.as_ref()
        } else {
            let hash = sqlx::query_scalar::<_, String>(
                "SELECT password_hash FROM users WHERE username = ? COLLATE NOCASE",
            )
            .bind(username)
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            hash.is_some_and(|hash| verify_password(password, &hash))
        };
        if !valid {
            return Err(AuthFailure::unauthorized(
                "invalid_credentials",
                "invalid username or password",
            ));
        }
        self.create_session(username, is_admin).await
    }

    async fn create_session(
        &self,
        username: &str,
        is_admin: bool,
    ) -> Result<CurrentSession, AuthFailure> {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let expires_at = now + Duration::days(SESSION_TTL_DAYS);
        sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO sessions(token, username, is_admin, created_at, expires_at) \
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(&token)
        .bind(username)
        .bind(is_admin)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(CurrentSession {
            token,
            username: username.to_string(),
            is_admin,
        })
    }

    async fn session(&self, token: &str) -> Result<Option<CurrentSession>, AuthFailure> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query(
            "SELECT username, is_admin FROM sessions WHERE token = ? AND expires_at > ?",
        )
        .bind(token)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(row.map(|row| CurrentSession {
            token: token.to_string(),
            username: row.get("username"),
            is_admin: row.get::<i64, _>("is_admin") != 0,
        }))
    }

    async fn logout(&self, token: &str) -> Result<(), AuthFailure> {
        sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(())
    }

    async fn create_invitation(&self, created_by: &str) -> Result<(String, Url), AuthFailure> {
        let token = format!("inv_{}", Uuid::new_v4().simple());
        sqlx::query("INSERT INTO invitations(token, created_at, created_by) VALUES(?, ?, ?)")
            .bind(&token)
            .bind(Utc::now().to_rfc3339())
            .bind(created_by)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        let mut url = self.public_url.clone();
        url.set_path("/");
        url.query_pairs_mut().clear().append_pair("invite", &token);
        Ok((token, url))
    }

    async fn register(
        &self,
        invite: &str,
        username: &str,
        password: &str,
    ) -> Result<CurrentSession, AuthFailure> {
        let username = validate_username(username)?;
        if password.len() < 8 {
            return Err(AuthFailure::bad_request(
                "invalid_password",
                "password must contain at least 8 characters",
            ));
        }
        let available = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM invitations WHERE token = ? AND used_at IS NULL",
        )
        .bind(invite)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if available == 0 {
            return Err(AuthFailure::bad_request(
                "invalid_invitation",
                "invitation is invalid or already used",
            ));
        }
        let password_hash = hash_password(password)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let user_id = format!("usr_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO users(id, username, password_hash, created_at) VALUES(?, ?, ?, ?)",
        )
        .bind(&user_id)
        .bind(&username)
        .bind(password_hash)
        .bind(&now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("username_exists", "username is already registered")
            } else {
                AuthFailure::database(error)
            }
        })?;
        let result = sqlx::query(
            "UPDATE invitations SET used_at = ?, used_by = ? WHERE token = ? AND used_at IS NULL",
        )
        .bind(&now)
        .bind(&user_id)
        .bind(invite)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::bad_request(
                "invalid_invitation",
                "invitation is invalid or already used",
            ));
        }
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.create_session(&username, false).await
    }
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

pub async fn require_admin(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    match authenticate_request(&auth, request.headers()).await {
        Ok(session) if session.is_admin => {
            request.extensions_mut().insert(session);
            next.run(request).await
        }
        Ok(_) => AuthFailure::forbidden("admin_required", "administrator access required")
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
            username: "local".to_string(),
            is_admin: true,
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
    let session = auth.login(&request.username, &request.password).await?;
    Ok(session_response(&auth, &session))
}

pub async fn register(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<RegisterRequest>,
) -> Result<Response, AuthFailure> {
    let session = auth
        .register(&request.invite, &request.username, &request.password)
        .await?;
    Ok(session_response(&auth, &session))
}

pub async fn me(Extension(session): Extension<CurrentSession>) -> Json<Value> {
    Json(json!({ "username": session.username, "is_admin": session.is_admin }))
}

pub async fn logout(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
) -> Result<Response, AuthFailure> {
    auth.logout(&session.token).await?;
    let cookie = format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
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
) -> Result<Json<Value>, AuthFailure> {
    let (token, url) = auth.create_invitation(&session.username).await?;
    Ok(Json(json!({ "token": token, "url": url.as_str() })))
}

fn session_response(auth: &AuthStore, session: &CurrentSession) -> Response {
    let cookie = format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        session.token,
        SESSION_TTL_DAYS * 24 * 60 * 60,
        secure_cookie_suffix(auth)
    );
    (
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "username": session.username, "is_admin": session.is_admin })),
    )
        .into_response()
}

fn secure_cookie_suffix(auth: &AuthStore) -> &'static str {
    if auth.public_url.scheme() == "https" {
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

fn validate_username(username: &str) -> Result<String, AuthFailure> {
    let username = username.trim();
    if !(3..=32).contains(&username.len())
        || !username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || username.eq_ignore_ascii_case("admin")
    {
        return Err(AuthFailure::bad_request(
            "invalid_username",
            "username must be 3-32 letters, numbers, dots, dashes, or underscores",
        ));
    }
    Ok(username.to_string())
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
}

impl IntoResponse for AuthFailure {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.error })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invitation_registration_and_login_round_trip() {
        let store = AuthStore::in_memory("owner-password").await;
        let admin = store
            .login("admin", "owner-password")
            .await
            .expect("admin login");
        assert!(admin.is_admin);
        let (invite, url) = store
            .create_invitation(&admin.username)
            .await
            .expect("invitation");
        assert!(url.as_str().contains(&invite));

        let registered = store
            .register(&invite, "alice", "password123")
            .await
            .expect("registration");
        assert_eq!(registered.username, "alice");
        assert!(!registered.is_admin);
        assert!(store.register(&invite, "bob", "password123").await.is_err());

        let login = store
            .login("ALICE", "password123")
            .await
            .expect("case-insensitive login");
        assert_eq!(login.username, "ALICE");
        assert!(!login.is_admin);
    }

    #[tokio::test]
    async fn logout_invalidates_the_session() {
        let store = AuthStore::in_memory("owner-password").await;
        let session = store
            .login("admin", "owner-password")
            .await
            .expect("admin login");
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
    async fn disabled_auth_injects_a_local_administrator() {
        let mut store = AuthStore::in_memory("owner-password").await;
        store.disabled = true;
        let session = authenticate_request(&store, &HeaderMap::new())
            .await
            .expect("local session");
        assert_eq!(session.username, "local");
        assert!(session.is_admin);
    }
}
