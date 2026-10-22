//! Private settings HTTP boundary. No transport credentials or access tokens
//! ever enter a response. Blocking Proxy calls hold a bounded admission permit,
//! including after a disconnected caller cancels its async request.
#[path = "email_threads.rs"]
pub mod email_threads;
use crate::{
    oauth::OAuthVault,
    owner_auth::{BindingManager, Error, OwnerAuth},
};
use axum::{
    body::{to_bytes, Body},
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
    Router,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

const SESSION: &str = "__Host-treer-endpoint-session";
const BROWSER: &str = "__Host-treer-endpoint-login";
const MANUAL: &str = "# Treer message hub\n\nPrivate workspace channel settings and endpoint status. Browsers receive the human interface.\n\n## Inspect\n\n`GET /health` reports process health. `GET /api/settings` requires a current verified workspace Agent or human owner and returns bounded endpoint metadata.\n\n## Authentication\n\nTreer-injected App identity takes precedence; an invalid supplied identity is rejected. Without an identity, open this App in a browser and use Treer OAuth. Opaque sessions remain HttpOnly; never paste tokens into URLs. Workspace Agents can manage bindings with their own verified Treer identity. Token editing, OAuth and metadata purge remain human-owner-only. See AGENT-API.md for the shared JSON API.\n\nPOST /api/configuration accepts confirmed binding create, pause, delete and target changes from workspace Agents or the human owner; recovery and metadata commands require the human owner. Owner resume requires a separate default-off operator gate, fresh Core metadata, ready Telegram identity and explicit owner declarations about profile and isolation. These declarations are not attestation by the App. State contains sensitive metadata: keep this Managed App workspace-only, use a dedicated OS user or container, and protect backups.\n";
#[derive(Default, Serialize)]
pub struct SettingsSnapshot {
    pub endpoints: Vec<EndpointSnapshot>,
    pub management_notices: crate::management::NoticeStatus,
    pub email: Option<crate::email_workers::Snapshot>,
    pub activation_enabled: bool,
    pub workers: crate::scheduler::WorkerStatus,
    pub audit_full: bool,
    pub emergency_pause_bindings: i64,
    pub telegram_accounts: Vec<crate::telegram_workers::Status>,
    pub telegram_tokens: Vec<crate::telegram_tokens::Slot>,
}
#[derive(Serialize)]
pub struct EndpointSnapshot {
    binding: BindingSnapshot,
    revision: i64,
    paused: bool,
    reason: String,
    observed: Option<ObservedSnapshot>,
    diagnostic: Option<DiagnosticSnapshot>,
    activity: crate::activation_state::Activity,
    isolation: Option<crate::recovery::IsolationChoice>,
}
#[derive(Serialize)]
struct BindingSnapshot {
    endpoint_id: String,
    generation: i64,
    recipient: String,
    expected_path: String,
    expected_server_id: String,
}
#[derive(Serialize)]
struct ObservedSnapshot {
    path: Option<String>,
}
#[derive(Serialize)]
struct DiagnosticSnapshot {
    version: String,
    state: String,
}
impl From<crate::hub::View> for EndpointSnapshot {
    fn from(view: crate::hub::View) -> Self {
        Self {
            activity: view.activity,
            isolation: view.isolation,
            binding: BindingSnapshot {
                endpoint_id: view.binding.endpoint_id,
                generation: view.binding.generation,
                recipient: view.binding.recipient,
                expected_path: view.binding.expected_path,
                expected_server_id: view.binding.expected_server_id,
            },
            revision: view.revision,
            paused: view.paused,
            reason: view.reason,
            observed: view.observed.map(|v| ObservedSnapshot { path: v.path }),
            diagnostic: view.diagnostic.map(|d| DiagnosticSnapshot {
                version: d.version,
                state: d.state,
            }),
        }
    }
}
fn agent_snapshot(id: &str, snapshot: &SettingsSnapshot) -> Value {
    let endpoints:Vec<_>=snapshot.endpoints.iter().map(|e|json!({"binding":{"endpoint_id":e.binding.endpoint_id,"generation":e.binding.generation,"recipient":e.binding.recipient,"expected_path":e.binding.expected_path,"expected_server_id":e.binding.expected_server_id},"revision":e.revision,"paused":e.paused,"reason":e.reason,"isolation":e.isolation})).collect();
    let accounts:Vec<_>=snapshot.telegram_accounts.iter().map(|a|json!({"account_id":a.account_id,"state":a.state,"bot_identity_validated":a.bot_identity_validated})).collect();
    json!({"actor_id":id,"principal_kind":"agent","owner_id":null,"settings":{"endpoints":endpoints,"telegram_accounts":accounts,"activation_enabled":snapshot.activation_enabled,"management_notices":snapshot.management_notices,"audit_full":snapshot.audit_full}})
}
fn snapshot_value(owner: &str, snapshot: SettingsSnapshot) -> Result<Value, ()> {
    if snapshot.endpoints.len() > 20
        || snapshot.telegram_accounts.len() > 20
        || snapshot.telegram_tokens.len() > 20
    {
        return Err(());
    }
    let value = json!({"owner_id":owner,"settings":snapshot});
    if value.to_string().len() > 512 * 1024 {
        return Err(());
    }
    Ok(value)
}
#[derive(Default)]
struct WriteGate(Mutex<HashMap<String, (Instant, u32)>>);
impl WriteGate {
    fn admit(&self, owner: &str, now: Instant) -> bool {
        self.admit_limit(owner, now, 10)
    }
    fn admit_limit(&self, owner: &str, now: Instant, limit: u32) -> bool {
        let Ok(mut slots) = self.0.lock() else {
            return false;
        };
        slots.retain(|_, (start, _)| {
            now.saturating_duration_since(*start) < Duration::from_secs(60)
        });
        if !slots.contains_key(owner) && slots.len() >= 128 {
            return false;
        }
        let (_, count) = slots.entry(owner.into()).or_insert((now, 0));
        if *count >= limit {
            return false;
        }
        *count += 1;
        true
    }
}
type Mutation = dyn Fn(
        &BindingManager,
        crate::configuration::Command,
    ) -> Result<crate::configuration::Outcome, crate::configuration::Failure>
    + Send
    + Sync;
type ManagementAudit = dyn Fn(&str, Option<&str>, u16) -> Result<(), ()> + Send + Sync;
type Snapshot = dyn Fn() -> Result<SettingsSnapshot, ()> + Send + Sync;
/// The snapshot closure must only read bounded App state, never perform network
/// calls or return message bodies/secrets. Authentication happens before it.
pub struct Settings {
    auth: Arc<OwnerAuth>,
    vault: OAuthVault,
    snapshot: Arc<Snapshot>,
    mutation: Option<Arc<Mutation>>,
    management_audit: Option<Arc<ManagementAudit>>,
    token_mutation: Option<Arc<crate::telegram_tokens::Mutation>>,
    email: Option<Arc<crate::email_threads::Service>>,
    slots: Arc<Semaphore>,
    writes: WriteGate,
    request_admission: crate::verification_gate::AuditGate,
    exports: WriteGate,
    token_attempts: WriteGate,
    html: &'static str,
    javascript: &'static str,
    stylesheet: &'static str,
    allowed_hosts: BTreeSet<String>,
}
impl Settings {
    pub fn new(
        auth: OwnerAuth,
        snapshot: Arc<Snapshot>,
        html: &'static str,
    ) -> Result<Self, Error> {
        // Host cookies cannot be isolated between Apps sharing the Proxy origin.
        // OAuth sessions are deliberately supported only on a dedicated root URL.
        if !auth.dedicated_settings_origin() {
            return Err(Error::Configuration);
        }
        let url = auth.settings_url();
        let host = url.host_str().ok_or(Error::Configuration)?;
        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        let mut allowed_hosts = BTreeSet::from([authority]);
        if url.port().is_none() {
            allowed_hosts.insert(format!(
                "{host}:{}",
                url.port_or_known_default().ok_or(Error::Configuration)?
            ));
        }
        Ok(Self {
            auth: Arc::new(auth),
            vault: OAuthVault::default(),
            snapshot,
            mutation: None,
            management_audit: None,
            token_mutation: None,
            email: None,
            slots: Arc::new(Semaphore::new(16)),
            writes: WriteGate::default(),
            request_admission: Default::default(),
            exports: WriteGate::default(),
            token_attempts: WriteGate::default(),
            html,
            javascript: "",
            stylesheet: "",
            allowed_hosts,
        })
    }
    /// Explicit trusted configuration, never a forwarded header. Core replaces
    /// ingress/tunnel Host with the App's registered virtual hostname.
    pub fn with_virtual_host(mut self, host: &str) -> Result<Self, Error> {
        let platform_host = host
            .strip_prefix("svc-")
            .and_then(|value| value.strip_suffix(".internal"))
            .is_some_and(|id| {
                id.len() == 32
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
        if host.len() > 253
            || !(host.ends_with(".treer") || platform_host)
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
        {
            return Err(Error::Configuration);
        }
        self.allowed_hosts.insert(host.to_ascii_lowercase());
        Ok(self)
    }
    pub fn with_loopback_listener(mut self, listener: SocketAddr) -> Result<Self, Error> {
        if !listener.ip().is_loopback() || listener.port() == 0 {
            return Err(Error::Configuration);
        }
        self.allowed_hosts.insert(listener.to_string());
        Ok(self)
    }
    pub fn with_configuration(mut self, mutation: Arc<Mutation>) -> Self {
        self.mutation = Some(mutation);
        self
    }
    pub fn with_management_audit(mut self, audit: Arc<ManagementAudit>) -> Self {
        self.management_audit = Some(audit);
        self
    }
    pub fn with_tokens(mut self, mutation: Arc<crate::telegram_tokens::Mutation>) -> Self {
        self.token_mutation = Some(mutation);
        self
    }
    pub fn with_assets(mut self, javascript: &'static str, stylesheet: &'static str) -> Self {
        self.javascript = javascript;
        self.stylesheet = stylesheet;
        self
    }
    pub fn with_email(mut self, service: Arc<crate::email_threads::Service>) -> Self {
        self.email = Some(service);
        self
    }
    pub fn router(self) -> Router {
        Router::new().fallback(handle).with_state(Arc::new(self))
    }
}
fn response(status: StatusCode, value: Value) -> Response {
    let mut r = Response::new(Body::from(value.to_string()));
    *r.status_mut() = status;
    r.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    r
}
fn error(e: Error) -> Response {
    let status = match e {
        Error::Required | Error::InvalidIdentity => StatusCode::UNAUTHORIZED,
        Error::OwnerRequired | Error::MemberRequired | Error::Csrf | Error::OAuthState => {
            StatusCode::FORBIDDEN
        }
        Error::RateLimited | Error::LoginCapacity => StatusCode::TOO_MANY_REQUESTS,
        Error::Proxy => StatusCode::SERVICE_UNAVAILABLE,
        Error::Configuration => StatusCode::INTERNAL_SERVER_ERROR,
    };
    response(
        status,
        json!({"error":{"code": e.to_string(), "message": e.to_string()}}),
    )
}
fn fixed(status: StatusCode, code: &str) -> Response {
    response(status, json!({"error":{"code":code,"message":code}}))
}
fn secure(mut r: Response) -> Response {
    for (name, value) in [
        ("cache-control", "no-store"), ("pragma", "no-cache"),
        ("referrer-policy", "no-referrer"), ("x-content-type-options", "nosniff"),
        ("content-security-policy", "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'"),
    ] { r.headers_mut().insert(name, HeaderValue::from_static(value)); }
    r
}
fn redirect(location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(value) => {
            let mut r = Response::new(Body::empty());
            *r.status_mut() = StatusCode::SEE_OTHER;
            r.headers_mut().insert("location", value);
            r
        }
        Err(_) => error(Error::Configuration),
    }
}
fn cookie(r: &mut Response, name: &str, value: &str, max_age: i64) {
    // Values are vault-generated URL-safe secrets, never arbitrary user data.
    let text = format!("{name}={value}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&text) {
        r.headers_mut().append("set-cookie", value);
    }
}
fn cookies(headers: &HeaderMap) -> Result<(Option<String>, Option<String>), Error> {
    let mut names = HashSet::new();
    let mut session = None;
    let mut browser = None;
    let mut bytes = 0usize;
    for value in headers.get_all("cookie") {
        let text = value.to_str().map_err(|_| Error::InvalidIdentity)?;
        bytes = bytes.saturating_add(text.len());
        if bytes > 8192 {
            return Err(Error::InvalidIdentity);
        }
        for entry in text.split(';') {
            let (name, value) = entry.trim().split_once('=').ok_or(Error::InvalidIdentity)?;
            if name.is_empty() || !names.insert(name.to_string()) {
                return Err(Error::InvalidIdentity);
            }
            if name == SESSION {
                session = Some(value.to_string());
            }
            if name == BROWSER {
                browser = Some(value.to_string());
            }
        }
    }
    Ok((session, browser))
}
fn wants_html(headers: &HeaderMap) -> bool {
    let mut choices = Vec::new();
    for value in headers.get_all("accept") {
        let Ok(text) = value.to_str() else { continue };
        for media in text.split(',') {
            let mut parts = media.split(';');
            let kind = parts.next().unwrap_or("").trim();
            if kind != "text/html" && kind != "text/markdown" {
                continue;
            }
            let quality = parts
                .find_map(|p| p.trim().strip_prefix("q="))
                .map(|q| q.parse::<f32>().unwrap_or(0.0))
                .unwrap_or(1.0);
            if quality.is_finite() && quality > 0.0 && quality <= 1.0 {
                choices.push((kind == "text/html", quality));
            }
        }
    }
    if let Some((html, _)) = choices
        .iter()
        .enumerate()
        .max_by(|(ai, a), (bi, b)| a.1.total_cmp(&b.1).then_with(|| bi.cmp(ai)))
        .map(|(_, v)| v)
    {
        return *html;
    }
    headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("Mozilla/"))
}
fn callback(query: &str) -> Result<(String, String), Error> {
    if query.len() > 4096 {
        return Err(Error::OAuthState);
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in reqwest::Url::parse(&format!("https://invalid.example/?{query}"))
        .map_err(|_| Error::OAuthState)?
        .query_pairs()
    {
        match key.as_ref() {
            "code" if code.is_none() => code = Some(value.into_owned()),
            "state" if state.is_none() => state = Some(value.into_owned()),
            _ => return Err(Error::OAuthState),
        }
    }
    Ok((
        code.filter(|v| !v.is_empty()).ok_or(Error::OAuthState)?,
        state.filter(|v| !v.is_empty()).ok_or(Error::OAuthState)?,
    ))
}
async fn handle(
    State(app): State<Arc<Settings>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
) -> Response {
    if request.method() != Method::GET || request.uri().path() != "/api/settings" {
        return handle_inner(State(app.clone()), ConnectInfo(peer), request).await;
    }
    let mut nonce = [0u8; 16];
    if getrandom::fill(&mut nonce).is_err() {
        return secure(error(Error::Proxy));
    }
    let id: String = nonce.iter().map(|v| format!("{v:02x}")).collect();
    // Cheap in-memory admission precedes the bounded nonblocking audit queue.
    match app.request_admission.admit(peer.ip()) {
        Ok(()) => (),
        Err(e) => {
            if let Some(audit) = &app.management_audit {
                let _ = audit(&id, None, 429);
            }
            return secure(error(e));
        }
    };

    if let Some(audit) = &app.management_audit {
        if audit(&id, None, 102).is_err() {
            return secure(error(Error::Proxy));
        }
    }
    let mut response = handle_inner(State(app.clone()), ConnectInfo(peer), request).await;
    if let Some(audit) = &app.management_audit {
        // No body/header identity is logged. A refused read has no authenticated
        // mutation actor; mutations record their verified principal separately.
        if audit(&id, None, response.status().as_u16()).is_err() {
            response = secure(error(Error::Proxy));
        }
    }
    if let Ok(value) = HeaderValue::from_str(&id) {
        response
            .headers_mut()
            .insert("x-endpoint-request-id", value);
    }
    response
}
async fn handle_inner(
    State(app): State<Arc<Settings>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
) -> Response {
    let (parts, body) = request.into_parts();
    if parts.headers.len() > 128
        || parts
            .headers
            .iter()
            .map(|(k, v)| k.as_str().len() + v.len())
            .sum::<usize>()
            > 16384
    {
        return secure(fixed(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "headers_too_large",
        ));
    }
    if parts.uri.path().starts_with("/api/email/") {
        return match &app.email {
            Some(service) => {
                service
                    .clone()
                    .handle(peer, parts, body, &app.allowed_hosts)
                    .await
            }
            None => secure(fixed(StatusCode::NOT_FOUND, "route_not_found")),
        };
    }
    if parts.method == Method::POST
        && matches!(
            parts.uri.path(),
            "/api/configuration" | "/api/telegram/token"
        )
    {
        return configuration_request(app, peer, parts, body).await;
    }
    // Refuse framed bodies before waiting for bytes or consuming verification
    // capacity. This includes chunked zero-byte bodies and slow-drip clients.
    if parts.headers.contains_key("transfer-encoding")
        || parts
            .headers
            .get_all("content-length")
            .iter()
            .any(|v| v.as_bytes() != b"0")
    {
        return secure(fixed(StatusCode::BAD_REQUEST, "request_body_not_allowed"));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(3), to_bytes(body, 1024)).await {
        Ok(Ok(bytes)) if bytes.is_empty() => {}
        _ => return secure(fixed(StatusCode::BAD_REQUEST, "request_body_not_allowed")),
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok());
    let Some(now) = now else {
        return secure(error(Error::Proxy));
    };
    let expensive = parts.method == Method::GET
        && matches!(
            parts.uri.path(),
            "/api/settings" | "/api/auth/start" | "/api/auth/callback"
        );
    if !expensive {
        return secure(dispatch(
            &app,
            parts.method,
            parts.uri.path(),
            parts.uri.query(),
            &parts.headers,
            peer,
            now,
        ));
    }
    let permit = match app.slots.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return secure(error(Error::RateLimited)),
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        secure(dispatch(
            &app,
            parts.method,
            parts.uri.path(),
            parts.uri.query(),
            &parts.headers,
            peer,
            now,
        ))
    })
    .await
    {
        Ok(r) => r,
        Err(_) => secure(error(Error::Proxy)),
    }
}
fn owner_for_mutation(
    app: &Settings,
    headers: &HeaderMap,
    peer: SocketAddr,
    now: i64,
    token_only: bool,
    audit_actor: &Mutex<Option<String>>,
) -> Result<BindingManager, Error> {
    let mut hosts = headers.get_all("host").iter();
    if !hosts
        .next()
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| app.allowed_hosts.contains(&v.to_ascii_lowercase()))
        || hosts.next().is_some()
    {
        return Err(Error::Csrf);
    }
    let values: Result<Vec<_>, _> = headers
        .iter()
        .map(|(k, v)| v.to_str().map(|v| (k.as_str(), v)))
        .collect();
    let values = values.map_err(|_| Error::InvalidIdentity)?;
    let (session, _) = cookies(headers)?;
    let token = session
        .as_deref()
        .map(|s| app.vault.session_token(s, now))
        .transpose()?
        .flatten();
    let browser_context = values.iter().any(|(key, _)| {
        key.eq_ignore_ascii_case("cookie")
            || key.eq_ignore_ascii_case("origin")
            || key.to_ascii_lowercase().starts_with("sec-fetch-")
    });
    let explicit_identity = values.iter().any(|(key, _)| {
        key.eq_ignore_ascii_case("x-treer-identity-token")
            || key.eq_ignore_ascii_case("authorization")
    });
    // Reject browser CSRF and unauthenticated unsafe requests cheaply, before
    // Proxy verification. Cookie-free explicit Agents retain their API path.
    if browser_context || (!explicit_identity && token.is_none()) {
        app.auth.check_mutation(&values)?;
    }
    if token_only {
        app.auth.check_mutation(&values)?;
        return app
            .auth
            .authenticate_from(&values, token.as_deref(), now, peer.ip())
            .map(BindingManager::HumanOwner);
    }
    let manager = app
        .auth
        .authenticate_manager_from(&values, token.as_deref(), now, peer.ip())?;
    if manager.kind() == "agent" {
        if let Ok(mut actor) = audit_actor.lock() {
            *actor = Some(manager.id().to_owned());
        }
    }
    app.auth.check_manager_mutation(&manager, &values)?;
    Ok(manager)
}
async fn configuration_request(
    app: Arc<Settings>,
    peer: SocketAddr,
    parts: axum::http::request::Parts,
    body: Body,
) -> Response {
    let mut nonce = [0u8; 16];
    if getrandom::fill(&mut nonce).is_err() {
        return secure(error(Error::Proxy));
    }
    let request_id: String = nonce.iter().map(|v| format!("{v:02x}")).collect();
    // Cheap in-memory admission precedes the bounded nonblocking audit queue.
    match app.request_admission.admit(peer.ip()) {
        Ok(()) => (),
        Err(e) => {
            if let Some(audit) = &app.management_audit {
                let _ = audit(&request_id, None, 429);
            }
            return secure(error(e));
        }
    };

    // Record admission before Proxy work; cancelled/unfinished calls stay 102.
    if let Some(audit) = &app.management_audit {
        if audit(&request_id, None, 102).is_err() {
            return secure(error(Error::Proxy));
        }
    }
    let actor = Arc::new(Mutex::new(None));
    let mut response =
        configuration_request_inner(app.clone(), peer, parts, body, actor.clone()).await;
    let actor = actor.lock().ok().and_then(|a| a.clone());
    if let Some(audit) = &app.management_audit {
        if audit(&request_id, actor.as_deref(), response.status().as_u16()).is_err() {
            response = secure(error(Error::Proxy));
        }
    }
    if let Ok(v) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-endpoint-request-id", v);
    }
    response
}
async fn configuration_request_inner(
    app: Arc<Settings>,
    peer: SocketAddr,
    parts: axum::http::request::Parts,
    body: Body,
    audit_actor: Arc<Mutex<Option<String>>>,
) -> Response {
    let token_request = parts.uri.path() == "/api/telegram/token";
    if parts.uri.query().is_some()
        || if token_request {
            app.token_mutation.is_none()
        } else {
            app.mutation.is_none()
        }
    {
        return secure(fixed(StatusCode::NOT_FOUND, "route_not_found"));
    }
    let lengths: Vec<_> = parts.headers.get_all("content-length").iter().collect();
    let size = lengths
        .first()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if parts.headers.contains_key("transfer-encoding")
        || lengths.len() != 1
        || !size.is_some_and(|n| n > 0 && n <= 16384)
        || parts
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            != Some("application/json")
    {
        return secure(fixed(StatusCode::BAD_REQUEST, "invalid_configuration_body"));
    }
    let permit = match app.slots.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return secure(error(Error::RateLimited)),
    };
    let auth_app = app.clone();
    let auth_actor = audit_actor.clone();
    // The blocking verification owns the slot even if this HTTP future is dropped.
    let verified = tokio::task::spawn_blocking(move || {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|t| i64::try_from(t.as_secs()).ok())
            .ok_or(Error::Proxy);
        let result = now.and_then(|now| {
            owner_for_mutation(
                &auth_app,
                &parts.headers,
                peer,
                now,
                token_request,
                &auth_actor,
            )
        });
        (result, permit)
    })
    .await;
    let (owner, permit) = match verified {
        Ok((Ok(owner), permit)) => (owner, permit),
        Ok((Err(e), _)) => return secure(error(e)),
        Err(_) => return secure(error(Error::Proxy)),
    };
    if owner.kind() == "agent" {
        if let Ok(mut actor) = audit_actor.lock() {
            *actor = Some(owner.id().to_owned());
        }
    }
    // Authenticate before reading JSON. The same slot remains held during the
    // three-second bounded read and all subsequent Core/configuration work.
    let bytes = match tokio::time::timeout(std::time::Duration::from_secs(3), to_bytes(body, 16384))
        .await
    {
        Ok(Ok(b)) if Some(b.len()) == size => b,
        _ => return secure(fixed(StatusCode::BAD_REQUEST, "invalid_configuration_body")),
    };
    if token_request {
        return match tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let command = serde_json::from_slice::<crate::telegram_tokens::Command>(&bytes);
            // Failed submissions count, including malformed JSON/shape. Removal
            // is containment and is never blocked by ordinary submission quotas.
            let removes = command.as_ref().is_ok_and(|c| c.removes());
            if !removes {
                let now = Instant::now();
                let owner_allowed = app.token_attempts.admit_limit(owner.id(), now, 5);
                let global_allowed = app.token_attempts.admit_limit("token:global", now, 20);
                let write_allowed = app.writes.admit(owner.id(), now);
                if !owner_allowed || !global_allowed || !write_allowed {
                    return fixed(StatusCode::TOO_MANY_REQUESTS, "telegram_token_rate_limited");
                }
            }
            let command = match command {
                Ok(c) => c,
                Err(_) => return fixed(StatusCode::BAD_REQUEST, "telegram_token_invalid"),
            };
            match app.token_mutation.as_ref().expect("checked token service")(
                match owner.owner() {
                    Ok(o) => o,
                    Err(e) => return error(e),
                },
                command,
            ) {
                Ok(slot) => response(
                    StatusCode::OK,
                    json!({"status":"telegram_token_saved","account":slot}),
                ),
                Err(e) => {
                    use crate::telegram_tokens::Failure;
                    let status = match e {
                        Failure::Invalid | Failure::Rejected => StatusCode::BAD_REQUEST,
                        Failure::DifferentBot | Failure::Active => StatusCode::CONFLICT,
                        Failure::Verification => StatusCode::BAD_GATEWAY,
                        Failure::Storage => StatusCode::SERVICE_UNAVAILABLE,
                        Failure::Expired => StatusCode::UNAUTHORIZED,
                    };
                    fixed(status, &e.to_string())
                }
            }
        })
        .await
        {
            Ok(r) => secure(r),
            Err(_) => secure(error(Error::Proxy)),
        };
    }
    let command: crate::configuration::Command = match serde_json::from_slice(&bytes) {
        Ok(c) => c,
        Err(_) => return secure(fixed(StatusCode::BAD_REQUEST, "invalid_configuration_body")),
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if matches!(
            &command,
            crate::configuration::Command::ExportMetadata { .. }
        ) && !app.exports.admit_limit(owner.id(), Instant::now(), 6)
        {
            return fixed(StatusCode::TOO_MANY_REQUESTS, "owner_export_rate_limited");
        }
        if command.consumes_write_quota() && !app.writes.admit(owner.id(), Instant::now()) {
            return fixed(StatusCode::TOO_MANY_REQUESTS, "owner_write_rate_limited");
        }
        match (app
            .mutation
            .as_ref()
            .expect("checked configuration service"))(&owner, command)
        {
            Ok(outcome) => response(StatusCode::OK, json!(outcome)),
            Err(crate::configuration::Failure::Invalid) => {
                fixed(StatusCode::BAD_REQUEST, "invalid_configuration")
            }
            Err(crate::configuration::Failure::InvalidPreview(code)) => {
                fixed(StatusCode::BAD_REQUEST, code)
            }
            Err(crate::configuration::Failure::Conflict) => {
                fixed(StatusCode::CONFLICT, "configuration_conflict")
            }
            Err(crate::configuration::Failure::Held) => fixed(StatusCode::CONFLICT, "binding_held"),
            Err(crate::configuration::Failure::Unavailable) => {
                fixed(StatusCode::SERVICE_UNAVAILABLE, "configuration_unavailable")
            }
            Err(crate::configuration::Failure::Expired) => error(Error::InvalidIdentity),
            Err(crate::configuration::Failure::Forbidden) => error(Error::OwnerRequired),
        }
    })
    .await
    {
        Ok(r) => secure(r),
        Err(_) => secure(error(Error::Proxy)),
    }
}
fn dispatch(
    app: &Settings,
    method: Method,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    peer: SocketAddr,
    now: i64,
) -> Response {
    if method == Method::GET && path == "/health" && query.is_none() {
        return response(StatusCode::OK, json!({"status":"running"}));
    }
    let mut hosts = headers.get_all("host").iter();
    let valid_host = hosts
        .next()
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| app.allowed_hosts.contains(&host.to_ascii_lowercase()));
    if !valid_host || hosts.next().is_some() {
        return fixed(StatusCode::BAD_REQUEST, "host_not_allowed");
    }
    let values: Result<Vec<_>, _> = headers
        .iter()
        .map(|(k, v)| v.to_str().map(|v| (k.as_str(), v)))
        .collect();
    let Ok(values) = values else {
        return error(Error::InvalidIdentity);
    };
    let (session, browser) = match cookies(headers) {
        Ok(c) => c,
        Err(e) => return error(e),
    };
    if method == Method::GET && path == "/api/auth/callback" {
        let (code, state) = match callback(query.unwrap_or("")) {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        match app.vault.finish(
            &app.auth,
            &code,
            &state,
            browser.as_deref().unwrap_or(""),
            now,
        ) {
            Ok(login) => {
                let mut r = redirect(app.auth.settings_url().as_str());
                cookie(&mut r, BROWSER, "", 0);
                cookie(
                    &mut r,
                    SESSION,
                    login.cookie_secret(),
                    login.expires_at().saturating_sub(now),
                );
                return r;
            }
            Err(failure) => {
                let mut r = error(failure.error);
                if failure.clear_browser_cookie {
                    cookie(&mut r, BROWSER, "", 0);
                }
                return r;
            }
        }
    }
    if query.is_some() {
        return fixed(StatusCode::BAD_REQUEST, "query_not_allowed");
    }
    if method == Method::GET && path == "/api/endpoint/auth/config" {
        return response(
            StatusCode::OK,
            json!({"login_url":app.auth.settings_url().join("api/auth/start").map(|v|v.to_string()).ok(), "settings_url":app.auth.settings_url().as_str()}),
        );
    }
    if method == Method::GET && matches!(path, "/assets/app.js" | "/assets/app.css") {
        let (body, kind) = if path.ends_with(".js") {
            (app.javascript, "text/javascript; charset=utf-8")
        } else {
            (app.stylesheet, "text/css; charset=utf-8")
        };
        let mut r = Response::new(Body::from(body));
        r.headers_mut()
            .insert("content-type", HeaderValue::from_static(kind));
        return r;
    }
    if method == Method::GET && path == "/" {
        let html = wants_html(headers);
        let mut r = Response::new(Body::from(if html { app.html } else { MANUAL }));
        r.headers_mut().insert(
            "content-type",
            HeaderValue::from_static(if html {
                "text/html; charset=utf-8"
            } else {
                "text/markdown; charset=utf-8"
            }),
        );
        r.headers_mut()
            .insert("vary", HeaderValue::from_static("Accept, User-Agent"));
        return r;
    }
    // Logout revokes only the opaque cookie that this browser already holds.
    // It needs CSRF, but no Proxy authority: expired/revoked owners must still
    // be able to discard their local session and clear their browser cookie.
    if method == Method::POST && path == "/api/endpoint/auth/logout" {
        if let Err(e) = app.auth.check_mutation(&values) {
            return error(e);
        }
        if let Some(raw) = session {
            if let Err(e) = app.vault.logout(&raw) {
                return error(e);
            }
        }
        let mut r = response(StatusCode::OK, json!({"logged_out":true}));
        cookie(&mut r, SESSION, "", 0);
        return r;
    }
    let token = match session
        .as_deref()
        .map(|s| app.vault.session_token(s, now))
        .transpose()
    {
        Ok(t) => t.flatten(),
        Err(e) => return error(e),
    };
    if method == Method::GET && path == "/api/auth/start" {
        // A supplied invalid identity cannot turn into a different login user.
        match app
            .auth
            .authenticate_from(&values, token.as_deref(), now, peer.ip())
        {
            Ok(_) => return redirect(app.auth.settings_url().as_str()),
            Err(Error::Required) => {}
            Err(e) => return error(e),
        }
        return match app.vault.start_from(&app.auth, now, peer.ip()) {
            Ok(start) => {
                let mut r = redirect(&start.location);
                cookie(&mut r, BROWSER, start.browser_secret(), 300);
                r
            }
            Err(e) => error(e),
        };
    }
    // Unknown routes and methods never become an HTML shell or perform Proxy
    // verification. In particular POST on a static URL cannot mutate state.
    if !(method == Method::GET && path == "/api/settings") {
        return fixed(StatusCode::NOT_FOUND, "route_not_found");
    }
    let owner = match app
        .auth
        .authenticate_manager_from(&values, token.as_deref(), now, peer.ip())
    {
        Ok(o) => o,
        Err(e) => return error(e),
    };
    match (app.snapshot)() {
        Ok(snapshot) if owner.kind() == "agent" => {
            if snapshot.endpoints.len() > 20 || snapshot.telegram_accounts.len() > 20 {
                return fixed(StatusCode::SERVICE_UNAVAILABLE, "settings_unavailable");
            }
            let value = agent_snapshot(owner.id(), &snapshot);
            if value.to_string().len() > 512 * 1024 {
                return fixed(StatusCode::SERVICE_UNAVAILABLE, "settings_unavailable");
            }
            response(StatusCode::OK, value)
        }
        Ok(snapshot) => match snapshot_value(owner.id(), snapshot) {
            Ok(mut value) => {
                value["principal_kind"] = json!(owner.kind());
                value["actor_id"] = json!(owner.id());
                if owner.kind() == "agent" {
                    value["owner_id"] = Value::Null;
                }
                response(StatusCode::OK, value)
            }
            Err(()) => fixed(StatusCode::SERVICE_UNAVAILABLE, "settings_unavailable"),
        },
        Err(_) => fixed(StatusCode::SERVICE_UNAVAILABLE, "settings_unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    const TOKEN: &str = "fixture.owner.token";
    fn app(proxy: &str) -> Settings {
        Settings::new(
            OwnerAuth::new(proxy, "svc_hub", "ws_test", "https://hub.example/").unwrap(),
            Arc::new(|| Ok(SettingsSnapshot::default())),
            "<!doctype html><title>Endpoints</title>",
        )
        .unwrap()
    }
    fn invoke(
        app: &Settings,
        method: Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> Response {
        let mut headers = headers.clone();
        if !headers.contains_key("host") {
            headers.insert("host", HeaderValue::from_static("hub.example"));
        }
        secure(dispatch(
            app,
            method,
            path,
            query,
            &headers,
            "127.0.0.1:1234".parse().unwrap(),
            1000,
        ))
    }
    fn headers(entries: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in entries {
            h.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }
    fn proxy(replies: Vec<Value>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let thread = thread::spawn(move || {
            for reply in replies {
                let start = std::time::Instant::now();
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(start.elapsed() < std::time::Duration::from_secs(2));
                            thread::sleep(std::time::Duration::from_millis(2));
                        }
                        Err(_) => panic!("fixture accept failed"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    head.push(byte[0]);
                    assert!(head.len() < 16384);
                }
                let text = String::from_utf8(head).unwrap();
                let size = text
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                assert!(size < 16384);
                let mut body = vec![0; size];
                stream.read_exact(&mut body).unwrap();
                let value = reply.to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",value.len(),value).unwrap();
            }
        });
        (base, thread)
    }
    fn verified() -> Vec<Value> {
        vec![
            json!({"active":true,"claims":{"sub":"user_owner","principal_kind":"human","workspace_id":"ws_test","service_id":"svc_hub","exp":2000}}),
            json!({"principals":[{"id":"user_owner","kind":"human","role":"owner"}]}),
        ]
    }
    #[test]
    fn authenticated_management_api_returns_fixed_held_error_without_core_io() {
        let (hub, _) = crate::recovery::tests::fixture(
            crate::store::Store::memory(Default::default()).unwrap(),
        );
        hub.store
            .db
            .execute(
                "INSERT INTO telegram_terminal_holds(endpoint,full) VALUES('ag_endpoint',1)",
                [],
            )
            .unwrap();
        let shared = Arc::new(Mutex::new(hub));
        let mut replies = verified();
        replies[0]["claims"]["exp"] = json!(4_000_000_000i64);
        let mut all_replies = replies.clone();
        all_replies.extend(replies);
        let (base, join) = proxy(all_replies);
        let state = shared.clone();
        let settings = Arc::new(
            app(&base).with_configuration(Arc::new(move |owner, command| {
                crate::configuration::execute(
                    &state,
                    &crate::recovery::tests::NoIo,
                    "ws_test",
                    owner,
                    command,
                )
            })),
        );
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            for command in [
                json!({"action":"preview_resume","endpoint_id":"ag_endpoint"}),
                json!({"action":"delete","endpoint_id":"ag_endpoint","revision":4,"confirm":true}),
            ] {
                let body = command.to_string();
                let request = axum::http::Request::builder()
                    .uri("/api/configuration").method(Method::POST)
                    .header("host", "hub.example")
                    .header("origin", "https://hub.example")
                    .header("x-endpoint-request", "1")
                    .header("x-treer-identity-token", TOKEN)
                    .header("content-type", "application/json")
                    .header("content-length", body.len())
                    .body(Body::from(body)).unwrap();
                let response = handle(State(settings.clone()), ConnectInfo("127.0.0.1:1234".parse().unwrap()), request).await;
                assert_eq!(response.status(), StatusCode::CONFLICT);
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({"error":{"code":"binding_held","message":"binding_held"}}));
            }
        });
        join.join().unwrap();
        let h = shared.lock().unwrap();
        assert!(h.view("ag_endpoint").unwrap().paused);
        assert_eq!(
            h.store
                .db
                .query_row("SELECT COUNT(*) FROM hub_retired_bindings", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn rejected_flood_is_bounded_without_taking_busy_hub_mutex() {
        let (hub, _) = crate::recovery::tests::fixture(
            crate::store::Store::memory(Default::default()).unwrap(),
        );
        let shared = Arc::new(Mutex::new(hub));
        let snapshot_hub = shared.clone();
        let mut settings = app("https://proxy.example/");
        settings.snapshot = Arc::new(move || {
            let _guard = snapshot_hub.try_lock().expect("rejected flood reached hub");
            Ok(SettingsSnapshot::default())
        });
        let temp = tempfile::tempdir().unwrap();
        let audit = Arc::new(
            crate::request_audit::RequestAudit::start(
                &temp.path().canonicalize().unwrap().join("audit"),
            )
            .unwrap(),
        );
        let queue = audit.clone();
        settings = settings.with_management_audit(Arc::new(move |id, actor, status| {
            queue.record(id, actor, status)
        }));
        let peer: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        for _ in 0..crate::verification_gate::AUDIT_REQUESTS {
            settings.request_admission.admit(peer.ip()).unwrap();
        }
        let settings = Arc::new(settings);
        let _busy = shared.lock().unwrap();
        let started = Instant::now();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for n in 0..100 {
                    let request = axum::http::Request::builder()
                        .uri(if n % 2 == 0 {
                            "/api/settings"
                        } else {
                            "/api/configuration"
                        })
                        .method(if n % 2 == 0 {
                            Method::GET
                        } else {
                            Method::POST
                        })
                        .body(Body::empty())
                        .unwrap();
                    assert_eq!(
                        handle(State(settings.clone()), ConnectInfo(peer), request)
                            .await
                            .status(),
                        StatusCode::TOO_MANY_REQUESTS
                    );
                }
            });
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn agent_snapshot_allowlist_omits_external_and_future_fields() {
        let snapshot = SettingsSnapshot::default();
        let value = agent_snapshot("ag_manager", &snapshot);
        assert!(value["settings"].get("email").is_none());
        assert!(value["settings"].get("telegram_tokens").is_none());
        assert_eq!(value["principal_kind"], "agent");
        assert!(value["owner_id"].is_null());
    }
    #[test]
    fn export_read_quota_is_independent_per_owner_and_resets_after_one_minute() {
        let exports = WriteGate::default();
        let writes = WriteGate::default();
        let now = Instant::now();
        for _ in 0..6 {
            assert!(exports.admit_limit("owner_a", now, 6));
        }
        assert!(!exports.admit_limit("owner_a", now, 6));
        assert!(exports.admit_limit("owner_b", now, 6));
        assert!(writes.admit("owner_a", now));
        assert!(exports.admit_limit("owner_a", now + Duration::from_secs(60), 6));
    }
    #[test]
    fn owner_write_quota_is_bounded_and_resets_without_blocking_safety_pause() {
        let gate = WriteGate::default();
        let now = Instant::now();
        for _ in 0..10 {
            assert!(gate.admit("user_owner", now));
        }
        assert!(!gate.admit("user_owner", now));
        assert!(gate.admit("user_other", now));
        assert!(gate.admit("user_owner", now + Duration::from_secs(60)));
        assert_eq!(gate.0.lock().unwrap().len(), 1);
        let pause: crate::configuration::Command = serde_json::from_value(
            json!({"action":"pause","endpoint_id":"ag_demo","revision":1,"confirm":true}),
        )
        .unwrap();
        assert!(!pause.consumes_write_quota());
        let bind:crate::configuration::Command=serde_json::from_value(json!({"action":"bind_telegram","endpoint_id":"ag_demo","revision":1,"confirm":true,"target":{"account_id":"bot_demo","chat_id":123,"topic_id":null,"allowed_senders":[456]}})).unwrap();
        assert!(bind.consumes_write_quota());
    }
    #[test]
    fn host_cookie_scope_requires_a_dedicated_root_url() {
        let auth = OwnerAuth::new(
            "https://proxy.example/",
            "svc_hub",
            "ws_test",
            "https://proxy.example/other-app/",
        )
        .unwrap();
        assert!(matches!(
            Settings::new(auth, Arc::new(|| Ok(SettingsSnapshot::default())), ""),
            Err(Error::Configuration)
        ));
        let shared = OwnerAuth::new(
            "https://proxy.example/",
            "svc_hub",
            "ws_test",
            "https://proxy.example/",
        )
        .unwrap();
        assert!(Settings::new(shared, Arc::new(|| Ok(SettingsSnapshot::default())), "").is_err());
    }
    #[test]
    fn snapshots_are_fixed_metadata_projection_bounded_to_twenty_and_512k() {
        let endpoint = |size| {
            EndpointSnapshot::from(crate::hub::View {
                activity: Default::default(),
                isolation: None,
                binding: crate::hub::Binding {
                    endpoint_id: "ag_demo".into(),
                    generation: 1,
                    recipient: "ag_recipient".into(),
                    expected_path: format!("/team/{}", "x".repeat(size)),
                    expected_cwd: "PRIVATE-CWD-UNRENDERED".into(),
                    expected_server_id: "machine_fixture".into(),
                },
                revision: 1,
                paused: true,
                reason: "owner_paused".into(),
                observed: None,
                checked_at: None,
                diagnostic: None,
            })
        };
        let value = snapshot_value(
            "user_owner",
            SettingsSnapshot {
                management_notices: Default::default(),
                email: None,
                endpoints: (0..20).map(|_| endpoint(4096)).collect(),
                workers: Default::default(),
                audit_full: false,
                emergency_pause_bindings: 0,
                telegram_accounts: vec![],
                telegram_tokens: vec![],
                activation_enabled: false,
            },
        )
        .unwrap();
        assert!(value.to_string().len() < 512 * 1024);
        assert!(!value.to_string().contains("PRIVATE-CWD-UNRENDERED"));
        assert!(!value.to_string().contains("expected_cwd"));
        assert!(snapshot_value(
            "user_owner",
            SettingsSnapshot {
                management_notices: Default::default(),
                email: None,
                endpoints: (0..21).map(|_| endpoint(1)).collect(),
                workers: Default::default(),
                audit_full: false,
                emergency_pause_bindings: 0,
                telegram_accounts: vec![],
                telegram_tokens: vec![],
                activation_enabled: false,
            }
        )
        .is_err());
        assert!(snapshot_value(
            "user_owner",
            SettingsSnapshot {
                management_notices: Default::default(),
                email: None,
                endpoints: vec![endpoint(512 * 1024)],
                workers: Default::default(),
                audit_full: false,
                emergency_pause_bindings: 0,
                telegram_accounts: vec![],
                telegram_tokens: vec![],
                activation_enabled: false,
            }
        )
        .is_err());
    }
    #[test]
    fn host_check_accepts_only_configured_dedicated_virtual_and_loopback_names() {
        let app = app("https://proxy.example/")
            .with_virtual_host("message-hub.treer")
            .unwrap()
            .with_virtual_host("svc_0123456789abcdef0123456789abcdef.treer")
            .unwrap()
            .with_virtual_host("svc-0123456789abcdef0123456789abcdef.internal")
            .unwrap()
            .with_loopback_listener("127.0.0.1:8786".parse().unwrap())
            .unwrap();
        for host in [
            "hub.example",
            "hub.example:443",
            "message-hub.treer",
            "svc_0123456789abcdef0123456789abcdef.treer",
            "svc-0123456789abcdef0123456789abcdef.internal",
            "SVC-0123456789ABCDEF0123456789ABCDEF.INTERNAL",
            "127.0.0.1:8786",
        ] {
            assert_eq!(
                invoke(
                    &app,
                    Method::GET,
                    "/api/settings",
                    None,
                    &headers(&[("host", host)])
                )
                .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        for host in [
            "evil.example",
            "svc_unconfigured.treer",
            "svc_0123456789abcdef0123456789abcdef.treer.evil",
            "svc-unconfigured.internal",
            "svc-0123456789abcdef0123456789abcdef.internal.evil",
            "127.0.0.1:9999",
            "hub.example.evil",
            "hub.example:8443",
        ] {
            assert_eq!(
                invoke(
                    &app,
                    Method::GET,
                    "/api/settings",
                    None,
                    &headers(&[("host", host)])
                )
                .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            invoke(
                &app,
                Method::GET,
                "/api/settings",
                None,
                &headers(&[("host", "hub.example"), ("host", "message-hub.treer")])
            )
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    #[test]
    fn root_negotiates_manual_or_browser_with_quality_and_order() {
        let app = app("https://proxy.example/");
        for (accept, agent, html) in [
            ("*/*", "curl/8", false),
            ("*/*", "Mozilla/5", true),
            ("text/markdown, text/html", "Mozilla/5", false),
            ("text/markdown;q=0.2,text/html;q=0.9", "curl/8", true),
            ("text/html;q=0.1,text/markdown;q=1", "Mozilla/5", false),
        ] {
            let r = invoke(
                &app,
                Method::GET,
                "/",
                None,
                &headers(&[("accept", accept), ("user-agent", agent)]),
            );
            assert_eq!(
                r.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("text/html"),
                html
            );
            assert_eq!(r.headers()["vary"], "Accept, User-Agent");
        }
    }
    #[test]
    fn callback_strips_code_sets_opaque_session_and_replay_fails_without_erasing_session() {
        let mut replies =
            vec![json!({"access_token":TOKEN,"token_type":"Bearer","audience":"svc_hub"})];
        replies.extend(verified());
        replies.extend(verified());
        let (base, join) = proxy(replies);
        let app = app(&base);
        let start = app.vault.start(&app.auth, 1000).unwrap();
        let parsed = reqwest::Url::parse(&start.location).unwrap();
        let state = parsed
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let query = format!("code=private-test-code&state={state}");
        let browser = format!("{BROWSER}={}", start.browser_secret());
        let r = invoke(
            &app,
            Method::GET,
            "/api/auth/callback",
            Some(&query),
            &headers(&[("cookie", &browser)]),
        );
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(r.headers()["location"], "https://hub.example/");
        let values = r
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        let session = values
            .iter()
            .find(|v| v.starts_with(SESSION))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        assert!(!session.contains(TOKEN));
        for v in values {
            for flag in ["Secure", "HttpOnly", "SameSite=Lax", "Path=/"] {
                assert!(v.contains(flag));
            }
        }
        let settings = invoke(
            &app,
            Method::GET,
            "/api/settings",
            None,
            &headers(&[("cookie", &session)]),
        );
        assert_eq!(settings.status(), StatusCode::OK);
        let replay = invoke(
            &app,
            Method::GET,
            "/api/auth/callback",
            Some(&query),
            &headers(&[("cookie", &browser)]),
        );
        assert_eq!(replay.status(), StatusCode::FORBIDDEN);
        assert!(!replay.headers().contains_key("set-cookie"));
        let secret = session.split_once('=').unwrap().1;
        assert!(app.vault.session_token(secret, 1000).unwrap().is_some());
        let logout = invoke(
            &app,
            Method::POST,
            "/api/endpoint/auth/logout",
            None,
            &headers(&[
                ("cookie", &session),
                ("origin", "https://hub.example"),
                ("x-endpoint-request", "1"),
            ]),
        );
        assert_eq!(logout.status(), StatusCode::OK);
        assert!(app.vault.session_token(secret, 1000).unwrap().is_none());
        join.join().unwrap();
    }
    #[test]
    fn duplicate_cookie_and_callback_parameters_are_rejected_without_clearing_valid_flow() {
        for q in [
            "code=a&code=b&state=c",
            "code=a&state=c&state=d",
            "code=a&state=c&redirect=https://evil.example",
            "error=access_denied",
            "code=&state=c",
        ] {
            assert!(callback(q).is_err());
        }
        assert!(cookies(&headers(&[("cookie", "x=1; x=2")])).is_err());
        assert!(cookies(&headers(&[
            ("cookie", "__Host-treer-endpoint-session=a"),
            ("cookie", "__Host-treer-endpoint-session=b")
        ]))
        .is_err());
        let app = app("https://proxy.example/");
        let result = invoke(
            &app,
            Method::GET,
            "/api/auth/callback",
            Some("code=x&state=wrong"),
            &headers(&[("cookie", "__Host-treer-endpoint-login=another-browser")]),
        );
        assert_eq!(result.status(), StatusCode::FORBIDDEN);
        assert!(!result.headers().contains_key("set-cookie"));
        assert_eq!(result.headers()["referrer-policy"], "no-referrer");
        assert_eq!(result.headers()["cache-control"], "no-store");
    }
    #[test]
    fn login_sets_only_secure_opaque_browser_cookie_and_invalid_injected_identity_cannot_fall_back()
    {
        let app = app("https://proxy.example/");
        let result = invoke(
            &app,
            Method::GET,
            "/api/auth/start",
            None,
            &HeaderMap::new(),
        );
        assert_eq!(result.status(), StatusCode::SEE_OTHER);
        let c = result.headers()["set-cookie"].to_str().unwrap();
        for flag in [
            "__Host-treer-endpoint-login=",
            "Path=/",
            "Secure",
            "HttpOnly",
            "SameSite=Lax",
            "Max-Age=300",
        ] {
            assert!(c.contains(flag));
        }
        let nonce = c.split(';').next().unwrap().split_once('=').unwrap().1;
        let location = result.headers()["location"].to_str().unwrap();
        assert!(location.starts_with("https://proxy.example/api/apps/oauth/authorize?"));
        assert!(!location.contains(nonce));
        assert!(location.contains("code_challenge_method=S256"));
        let bad = invoke(
            &app,
            Method::GET,
            "/api/auth/start",
            None,
            &headers(&[("x-treer-identity-token", "forged")]),
        );
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
        assert!(!bad.headers().contains_key("set-cookie"));
    }
    #[test]
    fn header_duplicates_are_authoritative_and_owner_snapshot_requires_current_verification() {
        let (base, join) = proxy(verified());
        let app = app(&base);
        let result = invoke(
            &app,
            Method::GET,
            "/api/settings",
            None,
            &headers(&[("x-treer-identity-token", TOKEN)]),
        );
        assert_eq!(result.status(), StatusCode::OK);
        join.join().unwrap();
        let bad = invoke(
            &app,
            Method::GET,
            "/api/settings",
            None,
            &headers(&[
                ("x-treer-identity-token", TOKEN),
                ("x-treer-identity-token", TOKEN),
            ]),
        );
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
        let missing = invoke(&app, Method::GET, "/api/settings", None, &HeaderMap::new());
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    }
    #[test]
    fn internal_virtual_host_is_registered_form_only_and_never_a_csrf_origin() {
        for host in [
            "localhost.internal",
            "svc-short.internal",
            "svc-0123456789abcdef0123456789abcdef.internal:80",
            "svc-0123456789ABCDEF0123456789ABCDEF.internal",
        ] {
            assert!(app("https://proxy.example/")
                .with_virtual_host(host)
                .is_err());
        }
        let app = app("https://proxy.example/")
            .with_virtual_host("svc-0123456789abcdef0123456789abcdef.internal")
            .unwrap();
        let result = invoke(
            &app,
            Method::POST,
            "/api/endpoint/auth/logout",
            None,
            &headers(&[
                ("host", "svc-0123456789abcdef0123456789abcdef.internal"),
                (
                    "origin",
                    "https://svc-0123456789abcdef0123456789abcdef.internal",
                ),
                ("x-endpoint-request", "1"),
            ]),
        );
        assert_eq!(result.status(), StatusCode::FORBIDDEN);
    }
    #[test]
    fn managed_service_host_does_not_grant_csrf_origin() {
        let host = "svc_0123456789abcdef0123456789abcdef.treer";
        let app = app("https://proxy.example/")
            .with_virtual_host(host)
            .unwrap();
        let result = invoke(
            &app,
            Method::POST,
            "/api/endpoint/auth/logout",
            None,
            &headers(&[
                ("host", host),
                (
                    "origin",
                    "http://svc_0123456789abcdef0123456789abcdef.treer",
                ),
                ("x-endpoint-request", "1"),
            ]),
        );
        assert_eq!(result.status(), StatusCode::FORBIDDEN);
    }
    #[test]
    fn logout_needs_csrf_but_clears_expired_session_without_proxy_authority() {
        let app = app("https://proxy.example/");
        let bad = invoke(
            &app,
            Method::POST,
            "/api/endpoint/auth/logout",
            None,
            &headers(&[
                ("x-treer-identity-token", TOKEN),
                ("origin", "https://evil.example"),
                ("x-endpoint-request", "1"),
            ]),
        );
        assert_eq!(bad.status(), StatusCode::FORBIDDEN);
        assert!(!bad.headers().contains_key("set-cookie"));
        let good = invoke(
            &app,
            Method::POST,
            "/api/endpoint/auth/logout",
            None,
            &headers(&[
                ("x-treer-identity-token", TOKEN),
                ("origin", "https://hub.example"),
                ("x-endpoint-request", "1"),
                ("sec-fetch-site", "same-origin"),
            ]),
        );
        assert_eq!(good.status(), StatusCode::OK);
        assert!(good.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));
    }
    #[test]
    fn unknown_routes_and_wrong_methods_never_become_html_or_redirects() {
        let app = app("https://proxy.example/");
        for (m, p, q) in [
            (Method::POST, "/", None),
            (Method::GET, "/api/missing", None),
            (Method::GET, "/api/endpoint/auth/logout", None),
            (Method::GET, "/api/settings", Some("token=untrusted")),
        ] {
            let r = invoke(&app, m, p, q, &HeaderMap::new());
            assert!(r.status().is_client_error());
            assert!(r.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("application/json"));
            assert!(!r.headers().contains_key("location"));
        }
    }
    #[test]
    fn real_router_bounds_headers_body_and_concurrent_blocking_admission() {
        let app = app("https://proxy.example/");
        let occupied = app.slots.clone().try_acquire_many_owned(16).unwrap();
        let router = app.router();
        let (send, recv) = std::sync::mpsc::channel();
        let (stop, done) = tokio::sync::oneshot::channel::<()>();
        let server = thread::spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    send.send(listener.local_addr().unwrap()).unwrap();
                    axum::serve(
                        listener,
                        router.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .with_graceful_shutdown(async {
                        let _ = done.await;
                    })
                    .await
                    .unwrap();
                });
        });
        let base = format!(
            "http://{}",
            recv.recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
        );
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        let result = client.get(format!("{base}/health")).send().unwrap();
        assert_eq!(result.status(), StatusCode::OK);
        let limited = client.get(format!("{base}/api/settings")).send().unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        drop(occupied);
        let result = client.get(format!("{base}/health")).send().unwrap();
        assert_eq!(result.status(), StatusCode::OK);
        let result = client
            .post(format!("{base}/api/endpoint/auth/logout"))
            .body(vec![b'x'; 2048])
            .send()
            .unwrap();
        assert_eq!(result.status(), StatusCode::BAD_REQUEST);
        let result = client
            .get(format!("{base}/api/settings"))
            .header("x-fixture", "x".repeat(17000))
            .send()
            .unwrap();
        assert_eq!(result.status(), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
        let address = base.strip_prefix("http://").unwrap();
        let mut drip = std::net::TcpStream::connect(address).unwrap();
        drip.set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        write!(drip,"POST /api/endpoint/auth/logout HTTP/1.1\r\nHost: hub.example\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
        let mut reply = [0; 256];
        let read = drip.read(&mut reply).unwrap();
        assert!(std::str::from_utf8(&reply[..read])
            .unwrap()
            .starts_with("HTTP/1.1 400"));
        drop(drip);
        stop.send(()).unwrap();
        server.join().unwrap();
    }
}
