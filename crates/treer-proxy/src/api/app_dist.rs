//! Optional same-process serving of the browser App build (#132, single
//! container). Opt-in through `--app-dist-dir`; when enabled the Proxy answers
//! requests whose Host is the configured App public URL with files from that
//! directory (SPA fallback to `index.html`), the runtime `/config.json`
//! document and the App health probe, carrying the same fixed security headers
//! as the standalone nginx image. Reserved Core prefixes are never served as
//! App content, files are resolved only inside the canonical directory (no
//! traversal, no symlink escape), and when the App shares the Proxy origin the
//! embedded App-UI and virtual-host tunnels are refused with a fixed code
//! because App content must never run on the Console's origin.
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{header, HeaderValue, Method, Request, Response};
use serde_json::json;
use url::Url;

use super::errors::ApiFailure;
use super::network::{proxy_service_ingress, request_hostname};
use super::{BootstrapConfig, IngressConfig};
use crate::auth::AuthStore;
use crate::identity::IdentityIssuer;
use crate::policy::PolicyEngine;
use crate::state::AppState;

/// Path prefixes that belong to Core routes. An unmatched request below them
/// is a JSON 404, never `index.html`, so a typo in an API path cannot look like
/// a page and the SPA cannot shadow future Core routes.
const RESERVED_PREFIXES: [&str; 6] = [
    "/api/",
    "/agent/",
    "/artifacts/",
    "/.treer/",
    "/.well-known/",
    "/install.",
];

#[derive(Clone)]
pub struct AppDistConfig {
    inner: Option<Arc<AppDistInner>>,
}

struct AppDistInner {
    dist: PathBuf,
    app: Authority,
    proxy: Authority,
    proxy_url: String,
    embedding_allowed: bool,
}

/// Normalized host:port of a configured public URL: lower-case host, trailing
/// dot removed, default port implied by the scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Authority {
    host: String,
    port: u16,
    /// The scheme's default port: a Host header without a port means this
    /// port, so `localhost` never matches a URL configured as `localhost:8080`.
    scheme_default: u16,
}

impl Authority {
    fn of(url: &Url) -> anyhow::Result<Self> {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("public URL {url} has no host"))?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow::anyhow!("public URL {url} has no port"))?;
        let scheme_default = match url.scheme() {
            "https" => 443,
            "http" => 80,
            other => anyhow::bail!("public URL scheme {other} is not http or https"),
        };
        Ok(Self {
            host,
            port,
            scheme_default,
        })
    }

    /// Exact host and port; a missing request port is the scheme default.
    fn matches(&self, host: &str, port: Option<u16>) -> bool {
        self.host == host && port.unwrap_or(self.scheme_default) == self.port
    }
}

/// Embedded App-UI and virtual-host tunnel paths, refused before any handler
/// when the App shares the Proxy origin.
fn is_embedded_tunnel_path(path: &str) -> bool {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    matches!(
        parts.as_slice(),
        ["api", "workspaces", _, "agents", _, "interface", "ui", ..]
    ) || matches!(
        parts.as_slice(),
        ["api", "workspaces", _, "virtual-hosts", _, "proxy", ..]
    )
}

/// Which configured public name a request addressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::api) enum HostClass {
    /// The App public URL (Console pages); when App and Proxy share an origin
    /// this is `Both`.
    App,
    Proxy,
    Both,
    Ingress,
    Other,
}

impl AppDistConfig {
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// `dist_dir` must exist and contain `index.html`; it is canonicalized once
    /// so later lookups compare physical paths.
    pub fn new(
        dist_dir: &Path,
        app_public_url: &Url,
        proxy_public_url: &Url,
    ) -> anyhow::Result<Self> {
        let dist = std::fs::canonicalize(dist_dir).map_err(|error| {
            anyhow::anyhow!("app dist directory {}: {error}", dist_dir.display())
        })?;
        if !dist.join("index.html").is_file() {
            anyhow::bail!(
                "app dist directory {} has no index.html",
                dist_dir.display()
            );
        }
        let app = Authority::of(app_public_url)?;
        let proxy = Authority::of(proxy_public_url)?;
        let embedding_allowed = app_public_url.origin() != proxy_public_url.origin();
        let mut proxy_url = proxy_public_url.clone();
        proxy_url.set_path("/");
        proxy_url.set_query(None);
        proxy_url.set_fragment(None);
        Ok(Self {
            inner: Some(Arc::new(AppDistInner {
                dist,
                app,
                proxy,
                proxy_url: proxy_url.to_string(),
                embedding_allowed,
            })),
        })
    }

    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// False only when the Console is served by the Proxy on the Proxy's own
    /// origin: embedded App content would then share the Console origin.
    pub fn embedding_allowed(&self) -> bool {
        self.inner
            .as_ref()
            .is_none_or(|inner| inner.embedding_allowed)
    }

    pub(in crate::api) fn require_embedding_allowed(&self) -> Result<(), ApiFailure> {
        if self.embedding_allowed() {
            return Ok(());
        }
        Err(embedding_refused())
    }

    /// Classifies the request's single Host (or HTTP/2 authority) against the
    /// configured names. Disabled configurations classify everything as
    /// `Other`, which the callers treat as "no App serving" without refusing.
    pub(in crate::api) fn host_class(
        &self,
        headers: &axum::http::HeaderMap,
        uri: &axum::http::Uri,
        ingress: &IngressConfig,
    ) -> Result<HostClass, ApiFailure> {
        let authority = request_authority(headers, uri)?;
        let host = authority.host().trim_end_matches('.').to_ascii_lowercase();
        let port = authority.port_u16();
        let Some(inner) = self.inner.as_ref() else {
            return Ok(HostClass::Other);
        };
        let app = inner.app.matches(&host, port);
        let proxy = inner.proxy.matches(&host, port);
        Ok(match (app, proxy) {
            (true, true) => HostClass::Both,
            (true, false) => HostClass::App,
            (false, true) => HostClass::Proxy,
            (false, false) if ingress.matches_hostname(&host) => HostClass::Ingress,
            (false, false) => HostClass::Other,
        })
    }
}

/// Exactly one Host header (or the HTTP/2 authority when no Host is present);
/// anything else is a fixed 400. Forwarded headers are never consulted.
fn request_authority(
    headers: &axum::http::HeaderMap,
    uri: &axum::http::Uri,
) -> Result<axum::http::uri::Authority, ApiFailure> {
    let hosts: Vec<_> = headers.get_all(header::HOST).iter().collect();
    match hosts.as_slice() {
        [] => uri
            .authority()
            .cloned()
            .ok_or_else(|| ApiFailure::bad_request("host_required", "Host header is required")),
        [host] => host
            .to_str()
            .map_err(|_| ApiFailure::bad_request("invalid_host", "Host header is invalid"))?
            .parse::<axum::http::uri::Authority>()
            .map_err(|_| ApiFailure::bad_request("invalid_host", "Host header is invalid")),
        _ => Err(ApiFailure::bad_request(
            "invalid_host",
            "exactly one Host header is required",
        )),
    }
}

fn embedding_refused() -> ApiFailure {
    ApiFailure::service_unavailable(
        "embedded_ui_requires_separate_app_origin",
        "Embedded App interfaces are unavailable while the App is served on the Proxy origin; configure a separate App public URL",
    )
}

fn host_not_configured() -> ApiFailure {
    ApiFailure {
        status: axum::http::StatusCode::MISDIRECTED_REQUEST,
        error: treer_protocol::ProtocolError::new(
            "host_not_configured",
            "this host is not one of the configured public URLs",
        ),
    }
}

/// Outermost middleware in App-serving mode: a strict Host allowlist (a DNS
/// rebinding defence for `docker run -p` setups) and route exclusivity. The
/// Console host never executes Core routes; unknown hosts get a fixed 421; the
/// Proxy and ingress hosts proceed unchanged. Disabled: pass-through, so the
/// Compose deployment keeps accepting any Host.
pub(in crate::api) async fn host_routing(
    State((bootstrap, ingress)): State<(BootstrapConfig, IngressConfig)>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response<Body> {
    let app = bootstrap.app_dist();
    if !app.enabled() {
        return next.run(request).await;
    }
    let class = match app.host_class(request.headers(), request.uri(), &ingress) {
        Ok(class) => class,
        Err(failure) => return axum::response::IntoResponse::into_response(failure),
    };
    if !app.embedding_allowed() && is_embedded_tunnel_path(request.uri().path()) {
        return axum::response::IntoResponse::into_response(embedding_refused());
    }
    match class {
        HostClass::Other => axum::response::IntoResponse::into_response(host_not_configured()),
        HostClass::App => {
            let path = request.uri().path();
            if RESERVED_PREFIXES
                .iter()
                .any(|prefix| path.starts_with(prefix))
            {
                return axum::response::IntoResponse::into_response(ApiFailure::not_found(
                    "route_not_found",
                    "route not found",
                ));
            }
            next.run(request).await
        }
        HostClass::Proxy | HostClass::Both | HostClass::Ingress => next.run(request).await,
    }
}

/// The outer router's fallback: service ingress hosts first (unchanged), then
/// the App build for the App host, otherwise a JSON 404.
pub(in crate::api) async fn app_or_ingress_fallback(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(ingress): Extension<IngressConfig>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(bootstrap): Extension<BootstrapConfig>,
    request: Request<Body>,
) -> Result<Response<Body>, ApiFailure> {
    let hostname = request_hostname(request.headers())?;
    if ingress.matches_hostname(&hostname) {
        return proxy_service_ingress(
            State(state),
            Extension(auth),
            Extension(policy),
            Extension(ingress),
            Extension(identity),
            request,
        )
        .await;
    }
    let app = bootstrap.app_dist();
    let class = app.host_class(request.headers(), request.uri(), &ingress)?;
    if !matches!(class, HostClass::App | HostClass::Both) {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    let inner = app
        .inner
        .as_ref()
        .expect("an App host class implies an enabled configuration");
    // Only owned method/path cross the awaits below: the request body is not
    // Sync, so holding the request would make this handler's future !Send.
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    drop(request);
    let csp = console_csp(&inner.proxy_url, ingress.base_domain_if_configured());
    serve_app(inner, &method, &path, &csp).await
}

/// The policy the Console always had, without a `frame-src`; the fallback when
/// a generated value cannot be a header.
const CLOSED_CONSOLE_CSP: &str = "frame-ancestors 'none'; object-src 'none'; base-uri 'none'";

/// The Console's `Content-Security-Policy` as served by the Proxy: the same
/// `frame-src` the Worker host emits for the same inputs (#156 incident 4890;
/// the nginx template sends no `frame-src` and is unchanged), so the embedded
/// Agent frame on `https://ag-<id>.<apps domain>` is allowed wherever the
/// Console is hosted by the Proxy or the Worker, and only there. Without an
/// apps domain the frame list stays `'self'` plus the Proxy origin.
pub(crate) fn console_csp(proxy_url: &str, apps_domain: Option<&str>) -> String {
    format!(
        "{}; frame-ancestors 'none'; object-src 'none'; base-uri 'none'",
        console_frame_src(proxy_url, apps_domain)
    )
}

/// `frame-src 'self' <Proxy origin>[ https://*.<apps domain>]`: the wildcard
/// is exactly one source, HTTPS only, directly under the configured apps base
/// domain (CSP host wildcards match every label depth below it, which is the
/// ingress domain itself; a sibling domain such as `apps2.example` is not a
/// match). The Worker generates the same text from the same inputs.
pub(crate) fn console_frame_src(proxy_url: &str, apps_domain: Option<&str>) -> String {
    let proxy_origin = Url::parse(proxy_url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| "'none'".to_string());
    match apps_domain
        .map(|domain| domain.trim_end_matches('.').to_ascii_lowercase())
        .filter(|domain| {
            !domain.is_empty() && !domain.contains(['/', ':', '*', ' ', ';', ',', '\'', '"'])
        }) {
        Some(domain) => format!("frame-src 'self' {proxy_origin} https://*.{domain}"),
        None => format!("frame-src 'self' {proxy_origin}"),
    }
}

async fn serve_app(
    inner: &AppDistInner,
    method: &Method,
    path: &str,
    csp: &str,
) -> Result<Response<Body>, ApiFailure> {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    if RESERVED_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    match path {
        "/health" => {
            return Ok(with_headers(
                json_response(json!({"service": "treer-app", "status": "ok"})),
                "no-store",
                csp,
            ))
        }
        "/config.json" => {
            return Ok(with_headers(
                json_response(json!({"proxy_url": inner.proxy_url})),
                "no-store",
                csp,
            ))
        }
        _ => {}
    }
    let file = match resolve_file(&inner.dist, path).await {
        Some(file) => file,
        None => inner.dist.join("index.html"),
    };
    let bytes = tokio::fs::read(&file).await.map_err(|error| {
        tracing::warn!(%error, "app build file unavailable");
        ApiFailure::service_unavailable("app_dist_unavailable", "browser App build is unavailable")
    })?;
    let content_type = content_type(&file);
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    Ok(with_headers(response, "no-cache", csp))
}

/// A request path becomes a file only when every component is a plain name and
/// the physical file lies inside the canonical dist directory; symlinks that
/// leave it, directories and anything else fall back to the SPA page.
async fn resolve_file(dist: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.strip_prefix('/')?;
    if relative.is_empty() {
        return None;
    }
    let mut candidate = dist.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_str()?;
                if name.is_empty()
                    || name.contains(['\\', '\0', ':'])
                    || name == "."
                    || name == ".."
                {
                    return None;
                }
                candidate.push(name);
            }
            _ => return None,
        }
    }
    let physical = tokio::fs::canonicalize(&candidate).await.ok()?;
    if !physical.starts_with(dist) {
        return None;
    }
    let metadata = tokio::fs::metadata(&physical).await.ok()?;
    metadata.is_file().then_some(physical)
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map" | "webmanifest") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn json_response(value: serde_json::Value) -> Response<Body> {
    let mut response = Response::new(Body::from(value.to_string()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

/// `Cross-Origin-Opener-Policy` for the Console document only (#182): a
/// cross-origin popup the Console opens, or that a Console frame opens, lands
/// in its own browsing context group and cannot reach the Console through
/// `opener` (same-origin popups may keep a compatible opener). It is set on
/// HTML responses alone; JSON, assets, APIs and tunnels never carry it.
const CROSS_ORIGIN_OPENER_POLICY: header::HeaderName =
    header::HeaderName::from_static("cross-origin-opener-policy");

/// The nginx template's header set, item for item; HTML documents also get
/// `Cross-Origin-Opener-Policy: same-origin`.
fn with_headers(
    mut response: Response<Body>,
    cache_control: &'static str,
    csp: &str,
) -> Response<Body> {
    let html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    let headers = response.headers_mut();
    if html {
        headers.insert(
            CROSS_ORIGIN_OPENER_POLICY,
            HeaderValue::from_static("same-origin"),
        );
    } else {
        headers.remove(CROSS_ORIGIN_OPENER_POLICY);
    }
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // A security header must never be missing: an unrepresentable generated
    // value (not expected from a parsed URL host) falls back to the closed
    // policy the Console always had.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(csp).unwrap_or_else(|_| HeaderValue::from_static(CLOSED_CONSOLE_CSP)),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    response
}
