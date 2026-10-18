//! Embed sessions (#156): the Console iframe loads an Agent interface on the
//! Agent's own `ag-<id>` origin instead of the Proxy-origin tunnel.
//!
//! The standalone flow cannot run inside a frame (the authorize step needs a
//! top-level navigation with the Console cookie, and the Agent origin's
//! cookies are third-party there), so the Console mints a one-time code with
//! its own session, hands it to a Proxy-owned bootstrap page on the Agent
//! origin through `postMessage`, and that page redeems it for a `Partitioned`
//! session cookie. Partitioning is keyed by the top-level site, so it is not
//! the barrier: the host-only `__Host-` cookie, `frame-ancestors` limited to
//! the Console origin on embed responses and the gateway's Fetch Metadata
//! rule are. Browsers that block the cookie fail closed at the check step and
//! the Console falls back (or, with `--agent-embed-fallback refuse`, offers a
//! new tab only).
use super::*;

/// The Console mints one code per iframe load (#156): human session only,
/// workspace member (non-members get the unknown-id answer), interface gate,
/// 30-second single-use code bound to the Agent origin and the bootstrap
/// nonce hash the Console relays from the frame.
pub(in crate::api) async fn mint_agent_interface_embed(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(config): Extension<IngressConfig>,
    session: Option<Extension<CurrentSession>>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    Json(request): Json<EmbedAuthorizationRequest>,
) -> Result<Response, ApiFailure> {
    let Some(Extension(session)) = session else {
        return Err(ApiFailure::unauthorized(
            "authentication_required",
            "a Console session is required",
        ));
    };
    if !config.embed_available() {
        return Err(ApiFailure::service_unavailable(
            "embed_not_configured",
            "embedded Agent origins need an HTTPS wildcard ingress domain",
        ));
    }
    if !auth::valid_ingress_nonce(&request.browser_nonce_hash) {
        return Err(ApiFailure::bad_request(
            "invalid_request",
            "browser_nonce_hash must be a 64-character lowercase hex digest",
        ));
    }
    // A signed-in non-member gets the same answer as an unknown id (#164 a).
    auth.require_workspace_member(&workspace_id, &session.user_id)
        .await
        .map_err(|failure| {
            let (status, error) = failure.into_parts();
            if status == StatusCode::FORBIDDEN {
                ApiFailure::not_found("agent_not_found", "agent does not exist")
            } else {
                ApiFailure { status, error }
            }
        })?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
    require_agent_interface_access(
        &policy,
        &workspace_id,
        &PolicySubject::Human {
            user_id: session.user_id.clone(),
        },
        &agent,
    )
    .await?;
    if agent
        .interface
        .as_ref()
        .and_then(|interface| interface.ui_path.as_deref())
        .is_none()
    {
        return Err(ApiFailure::not_found(
            "agent_interface_ui_unavailable",
            &agent.agent_id,
        ));
    }
    let origin = config
        .agent_interface_origin_url(&agent.agent_id)
        .ok_or_else(|| {
            ApiFailure::service_unavailable(
                "embed_not_configured",
                "embedded Agent origins need an HTTPS wildcard ingress domain",
            )
        })?;
    // Minting is bounded per user (security 4750 P3-A): the Agent page in the
    // frame can replay 'ready' and have the Console ask again; it gains no
    // authority, so the bound only keeps the table and the traffic finite.
    if !state.upload_gate().allow(
        &format!("embed-mint:{}", session.user_id),
        EMBED_MINTS_PER_USER_PER_MINUTE,
    ) || auth.outstanding_embed_codes(&session.user_id).await?
        >= EMBED_OUTSTANDING_CODES_PER_USER
    {
        return Err(ApiFailure {
            status: StatusCode::TOO_MANY_REQUESTS,
            error: ProtocolError::new(
                "embed_rate_limited",
                "too many embed authorizations for this user; retry in a minute",
            ),
        });
    }
    let base = config.base_domain()?;
    let ingress = auth
        .ensure_agent_interface_ingress(&workspace_id, &agent.agent_id, &session.user_id, base)
        .await?;
    let code = auth
        .create_embed_auth_code(
            &ingress,
            &session.user_id,
            &agent.agent_id,
            &request.browser_nonce_hash,
        )
        .await?;
    let mut response = Json(json!({
        "code": code,
        "origin": origin.trim_end_matches('/'),
        "expires_in": 30,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Debug, serde::Deserialize)]
pub(in crate::api) struct EmbedAuthorizationRequest {
    pub browser_nonce_hash: String,
}

#[derive(Debug, serde::Deserialize)]
struct EmbedSessionRequest {
    code: String,
    nonce: String,
}

const EMBED_MINTS_PER_USER_PER_MINUTE: usize = 30;
const EMBED_OUTSTANDING_CODES_PER_USER: i64 = 8;
const EMBED_BOOTSTRAP_PATH: &str = "/.treer/embed";
const EMBED_SESSION_PATH: &str = "/.treer/embed/session";
const EMBED_CHECK_PATH: &str = "/.treer/embed/check";
const EMBED_BODY_LIMIT: usize = 4 * 1024;

/// Serves the embed routes of an Agent origin, or `None` when `path` is not
/// one of them. Existence of the Agent is never revealed here: the bootstrap
/// is a fixed page, and session and check answer the same generic failure
/// for an unknown Agent, a wrong code and a wrong nonce.
pub(in crate::api) async fn serve_agent_embed_route(
    auth: &AuthStore,
    config: &IngressConfig,
    hostname: &str,
    agent_id: &str,
    request: &mut Request<Body>,
) -> Option<Result<Response, ApiFailure>> {
    let path = request.uri().path();
    if path == EMBED_BOOTSTRAP_PATH {
        return Some(bootstrap_page(config, request));
    }
    if path == EMBED_SESSION_PATH {
        return Some(open_embed_session(auth, config, hostname, agent_id, request).await);
    }
    if path == EMBED_CHECK_PATH {
        // Only the method and headers cross the await: a `&Request<Body>`
        // held there would make the gateway's future !Send (Body is !Sync).
        let method = request.method().clone();
        let headers = request.headers().clone();
        return Some(check_embed_session(auth, config, hostname, &method, &headers).await);
    }
    None
}

fn no_store(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
}

/// The Proxy-owned page in the frame. It knows nothing secret: it generates a
/// nonce, announces its hash to the Console (exact origin), receives the code
/// from the Console (exact origin), redeems both on its own origin, verifies
/// that the partitioned cookie came back, then navigates to the Agent UI.
/// Anything else reports `cookies_blocked` to the Console and stops.
fn bootstrap_page(config: &IngressConfig, request: &Request<Body>) -> Result<Response, ApiFailure> {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    if !config.embed_available() {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    let console = config.console_origin();
    let script_nonce = Uuid::new_v4().simple().to_string();
    let console_js = serde_json::to_string(&console)
        .map_err(|error| ApiFailure::internal("encode_error", &error.to_string()))?;
    let html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Treer</title></head><body>
<script nonce="{script_nonce}">
(function () {{
  var CONSOLE = {console_js};
  var parentWindow = window.parent;
  var done = false;
  function report(kind) {{
    if (done) return; done = true;
    try {{ parentWindow.postMessage({{ treer_embed: kind }}, CONSOLE); }} catch (e) {{}}
  }}
  if (window.top === window || !window.crypto || !window.crypto.subtle) {{ report("cookies_blocked"); return; }}
  var bytes = new Uint8Array(32); window.crypto.getRandomValues(bytes);
  var nonce = Array.prototype.map.call(bytes, function (b) {{ return ("0" + b.toString(16)).slice(-2); }}).join("");
  window.crypto.subtle.digest("SHA-256", new TextEncoder().encode(nonce)).then(function (digest) {{
    var hash = Array.prototype.map.call(new Uint8Array(digest), function (b) {{ return ("0" + b.toString(16)).slice(-2); }}).join("");
    var timer = setTimeout(function () {{ report("cookies_blocked"); }}, 15000);
    window.addEventListener("message", function (event) {{
      if (done || event.origin !== CONSOLE || event.source !== parentWindow) return;
      var data = event.data;
      if (!data || data.treer_embed !== "code" || typeof data.code !== "string") return;
      clearTimeout(timer);
      fetch("/.treer/embed/session", {{ method: "POST", credentials: "include", redirect: "manual",
        headers: {{ "content-type": "application/json" }}, body: JSON.stringify({{ code: data.code, nonce: nonce }}) }})
      .then(function (r) {{ if (r.status !== 204) throw new Error("session"); return fetch("/.treer/embed/check", {{ credentials: "include", redirect: "manual", cache: "no-store" }}); }})
      .then(function (r) {{ if (r.status !== 204) throw new Error("check"); done = true; window.location.replace("/" + window.location.search); }})
      .catch(function () {{ report("cookies_blocked"); }});
    }});
    parentWindow.postMessage({{ treer_embed: "ready", nonce_hash: hash }}, CONSOLE);
  }}).catch(function () {{ report("cookies_blocked"); }});
}})();
</script></body></html>"#
    );
    let mut response = Response::new(Body::from(html));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&format!(
            "default-src 'none'; script-src 'nonce-{script_nonce}'; connect-src 'self'; \
             frame-ancestors {console}; base-uri 'none'; form-action 'none'"
        ))
        .map_err(|error| ApiFailure::internal("csp_error", &error.to_string()))?,
    );
    no_store(&mut response);
    Ok(response)
}

/// `POST /.treer/embed/session` on the Agent origin: same-origin browser
/// request (Fetch Metadata), JSON `{code, nonce}`, answers 204 with the
/// partitioned embed cookie or the generic invalid-authorization failure.
async fn open_embed_session(
    auth: &AuthStore,
    config: &IngressConfig,
    hostname: &str,
    agent_id: &str,
    request: &mut Request<Body>,
) -> Result<Response, ApiFailure> {
    if *request.method() != Method::POST {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    if !config.embed_available() {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    enforce_ingress_fetch_metadata(request, hostname, false)?;
    let body = axum::body::to_bytes(
        std::mem::replace(request.body_mut(), Body::empty()),
        EMBED_BODY_LIMIT,
    )
    .await
    .map_err(|_| ApiFailure::bad_request("invalid_request", "request body is too large"))?;
    let parsed: EmbedSessionRequest = serde_json::from_slice(&body)
        .map_err(|_| ApiFailure::bad_request("invalid_request", "expected {code, nonce}"))?;
    if parsed.code.len() > 128 || !auth::valid_ingress_nonce(&parsed.nonce) {
        return Err(auth::invalid_ingress_authorization().into());
    }
    let token = auth
        .consume_embed_auth_code(hostname, agent_id, &parsed.code, &parsed.nonce)
        .await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{}={token}; Path=/; Secure; HttpOnly; SameSite=None; Partitioned; Max-Age={}",
            config.embed_cookie_name(),
            12 * 60 * 60,
        ))
        .map_err(|error| ApiFailure::internal("cookie_error", &error.to_string()))?,
    );
    no_store(&mut response);
    Ok(response)
}

/// `GET /.treer/embed/check`: 204 when the request carries a live embed
/// session for this host, else 401; the bootstrap uses it to prove the
/// partitioned cookie is actually sent back before loading the Agent UI.
async fn check_embed_session(
    auth: &AuthStore,
    config: &IngressConfig,
    hostname: &str,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Response, ApiFailure> {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    let user = match cookie_value(headers, config.embed_cookie_name()) {
        Some(token) => auth.authenticate_embed_session(hostname, &token).await?,
        None => None,
    };
    if user.is_none() {
        return Err(ApiFailure::unauthorized(
            "ingress_authentication_required",
            "no embed session for this origin",
        ));
    }
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    no_store(&mut response);
    Ok(response)
}

/// Embed-session responses may be framed by the Console origin only; every
/// other `frame-ancestors` source and `X-Frame-Options` the Agent sent are
/// replaced, other directives stay. Standalone sessions are never rewritten.
pub(in crate::api) fn allow_console_frame_only(
    headers: &mut HeaderMap,
    console_origin: &str,
) -> Result<(), ApiFailure> {
    let ancestors = format!("frame-ancestors {console_origin}");
    let mut policies = Vec::new();
    for value in headers.get_all(header::CONTENT_SECURITY_POLICY) {
        let policy = value
            .to_str()
            .map_err(|error| ApiFailure::bad_gateway("invalid_agent_csp", &error.to_string()))?;
        let mut directives: Vec<&str> = policy
            .split(';')
            .map(str::trim)
            .filter(|directive| {
                !directive.is_empty()
                    && !directive
                        .split_whitespace()
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case("frame-ancestors"))
            })
            .collect();
        directives.push(&ancestors);
        policies.push(directives.join("; "));
    }
    if policies.is_empty() {
        policies.push(ancestors);
    }
    headers.remove(header::CONTENT_SECURITY_POLICY);
    for policy in policies {
        headers.append(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&policy).map_err(|error| {
                ApiFailure::bad_gateway("invalid_agent_csp", &error.to_string())
            })?,
        );
    }
    headers.remove(header::X_FRAME_OPTIONS);
    Ok(())
}

/// With `--agent-embed-fallback refuse`, the Proxy-origin interface tunnel
/// is closed wherever embed sessions are available, so a browser that cannot
/// hold the partitioned cookie gets "open in new tab" instead of the shared
/// origin (#156, security 4701 (8)).
pub(in crate::api) fn require_tunnel_fallback_allowed(
    config: &IngressConfig,
) -> Result<(), ApiFailure> {
    if config.embed_fallback() == EmbedFallback::Refuse && config.embed_available() {
        return Err(ApiFailure::service_unavailable(
            "embedded_ui_requires_isolated_origin",
            "this deployment embeds Agent interfaces on their own origin only",
        ));
    }
    Ok(())
}
