use super::{
    config::Config,
    controller::{Controller, Input},
    http::{self, Request, Response},
    owner::Verifier,
    preauth::{self, Budget, Lane},
};
use crate::{Binding, Error, Limits, Operation, Store};
use serde::Deserialize;
use serde_json::json;
use std::{
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
pub struct App {
    pub(super) config: Config,
    advertisement: Option<super::DataAdvertisement>,
    binding: Binding,
    gateway: Option<super::CliGateway>,
    gateway_credential: String,
    pub(super) store: Mutex<Store>,
    pub(super) controller: Controller,
    verifier: Verifier,
    connections: AtomicUsize,
    owner_connections: AtomicUsize,
    external: AtomicUsize,
    owners: AtomicUsize,
    containment: AtomicUsize,
    preauth: Mutex<Budget>,
    failures: Mutex<(Instant, u32)>,
}
struct Slot<'a>(&'a AtomicUsize);
impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}
fn slot(counter: &AtomicUsize, max: usize) -> Result<Slot<'_>, Error> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < max).then_some(n + 1)
        })
        .map(|_| Slot(counter))
        .map_err(|_| Error::Capacity)
}
impl App {
    #[cfg(test)]
    pub(super) fn test_public_connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(super) fn test_fill_slots(&self, full: bool) {
        self.owners
            .store(if full { 2 } else { 0 }, Ordering::Relaxed);
        self.external
            .store(if full { 8 } else { 0 }, Ordering::Relaxed);
    }
    /// Startup performs read-only authenticated Controller pin checks before binding a listener.
    pub fn open(config: Config, credential: String) -> Result<Arc<Self>, Error> {
        Self::open_advertised(config, credential, None)
    }
    pub fn open_advertised(
        config: Config,
        credential: String,
        advertisement: Option<super::DataAdvertisement>,
    ) -> Result<Arc<Self>, Error> {
        Self::open_gateway(config, credential, advertisement, None)
    }
    pub fn open_gateway(
        config: Config,
        credential: String,
        advertisement: Option<super::DataAdvertisement>,
        gateway: Option<super::CliGateway>,
    ) -> Result<Arc<Self>, Error> {
        if gateway.as_ref().is_some_and(|g| !g.matches(&config)) {
            return Err(Error::Invalid);
        }
        if advertisement
            .as_ref()
            .is_some_and(|ad| !ad.matches(&config))
        {
            return Err(Error::Invalid);
        }
        let binding = config.binding(&credential)?;
        let controller = Controller::new(config.clone(), credential.clone())?;
        controller.preflight()?;
        let store = Store::open(&config.state, binding.clone(), Limits::default())?;
        let verifier = Verifier::new(config.clone())?;
        Ok(Arc::new(Self {
            config,
            advertisement,
            binding,
            gateway,
            gateway_credential: credential,
            store: Mutex::new(store),
            controller,
            verifier,
            connections: AtomicUsize::new(0),
            owner_connections: AtomicUsize::new(0),
            external: AtomicUsize::new(0),
            owners: AtomicUsize::new(0),
            containment: AtomicUsize::new(0),
            preauth: Mutex::new(Budget::default()),
            failures: Mutex::new((Instant::now(), 0)),
        }))
    }
    pub fn serve(self: Arc<Self>, listener: TcpListener, owner: bool) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let stream = stream?;
            self.accept_lane(stream, owner);
        }
        Ok(())
    }
    /// Also used by the hermetic TCP fixtures; this is the real production parser/router.
    pub fn accept(self: &Arc<Self>, stream: TcpStream) {
        self.accept_lane(stream, false);
    }
    pub fn accept_owner(self: &Arc<Self>, stream: TcpStream) {
        self.accept_lane(stream, true);
    }
    fn connection_counter(&self, owner: bool) -> &AtomicUsize {
        if owner {
            &self.owner_connections
        } else {
            &self.connections
        }
    }
    fn accept_lane(self: &Arc<Self>, mut stream: TcpStream, owner: bool) {
        if slot(self.connection_counter(owner), if owner { 8 } else { 24 })
            .map(std::mem::forget)
            .is_err()
        {
            return;
        }
        let app = self.clone();
        let result = std::thread::Builder::new()
            .name("external-http".into())
            .spawn(move || {
                let _slot = Slot(app.connection_counter(owner));
                let response = match http::read(&mut stream) {
                    Ok(req) => app.route(req, owner),
                    Err(_) => Response::error(400, "invalid_request"),
                };
                let ancestors = format!(
                    "{} {}",
                    app.config.console_origin.origin().ascii_serialization(),
                    app.config.tunnel_origin.origin().ascii_serialization()
                );
                http::write(&mut stream, response, &ancestors);
            });
        if result.is_err() {
            self.connection_counter(owner)
                .fetch_sub(1, Ordering::Release);
        }
    }
    fn route(&self, r: Request, owner: bool) -> Response {
        match self.route_inner(r, owner) {
            Ok(v) => v,
            Err(e) => match e {
                Error::Denied => Response::error(401, "authentication_required"),
                Error::Disabled => Response::error(503, "disabled"),
                Error::Capacity => Response::error(429, "capacity_unavailable"),
                Error::Invalid => Response::error(400, "invalid_request"),
                _ => Response::error(503, "unavailable"),
            },
        }
    }
    fn route_inner(&self, mut r: Request, owner: bool) -> Result<Response, Error> {
        // Socket selection is operator configuration, never a caller-controlled header.
        if (r.path.starts_with("/owner/") && !owner) || (r.path.starts_with("/v1/") && owner) {
            return Ok(Response::error(404, "not_found"));
        }
        if r.path == "/api/tree" || r.path.starts_with("/api/tree?") {
            if !owner {
                return Ok(Response::error(404, "not_found"));
            }
            if r.method != "GET" {
                return Err(Error::Invalid);
            }
            let query = r.path.split_once('?').map(|(_, q)| q).unwrap_or("");
            let mut path = None;
            for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
                if k != "path" || path.is_some() || v.len() > 64 {
                    return Err(Error::Invalid);
                }
                path = Some(v.into_owned());
            }
            r.body = serde_json::to_vec(&json!({"path":path.unwrap_or_default()}))
                .map_err(|_| Error::Invalid)?;
            r.path = "/api/tree".into();
        }
        let inbox = super::inbox::Request::parse(&r)?;
        if inbox.is_none() {
            if let Some((path, query)) = r.path.split_once('?') {
                if r.method != "GET" || path != "/" || !appearance_query(query) {
                    return Err(Error::Invalid);
                }
                // Appearance hints never select credentials, owner scope or CSP ancestors.
                r.path = "/".to_owned();
            }
        }
        if r.method == "GET" {
            let asset = match r.path.as_str() {
                "/" => Some(root(&r, &self.config)),
                "/SKILL.md" => Some((
                    "text/markdown; charset=utf-8",
                    super::integration::manual_at(
                        &self.config,
                        self.advertisement.as_ref().map(|a| a.origin()),
                    ),
                )),
                "/settings.js" => Some((
                    "text/javascript; charset=utf-8",
                    include_str!("settings.js").to_owned(),
                )),
                "/settings.css" => Some((
                    "text/css; charset=utf-8",
                    include_str!("settings.css").to_owned(),
                )),
                "/health" => {
                    return Ok(Response::json(
                        200,
                        json!({"status":"ok","service":"external-agent"}),
                    ))
                }
                _ => None,
            };
            if let Some((kind, body)) = asset {
                return Ok(Response {
                    status: 200,
                    content_type: kind,
                    body: body.into_bytes(),
                });
            }
        }
        if r.path.starts_with("/owner/") || r.path == "/api/tree" {
            return self.owner_request(&r);
        }
        if !r.path.starts_with("/v1/") {
            return Ok(Response::error(404, "not_found"));
        }
        // No browser credentials or caller-provided workload/identity headers can enter this boundary.
        // Current public ingress adds x-treer-client-ip. It is ignored here, never
        // used for identity, authorization, token quotas or trusted peer attribution.
        if r.header("origin").is_some()
            || r.headers.keys().any(|k| {
                k.starts_with("sec-fetch-")
                    || (k.starts_with("x-treer-") && k != "x-treer-client-ip")
                    || k == "cookie"
            })
        {
            return Err(Error::Denied);
        }
        if r.path == "/v1/device/request" || r.path == "/v1/device/token" {
            if r.method != "POST" || r.header("authorization").is_some() {
                return Err(Error::Invalid);
            }
            self.preauth
                .lock()
                .map_err(|_| Error::PrivateStorage)?
                .check(r.peer, Lane::External)?;
            let _external = slot(&self.external, 8)?;
            require_json(&r)?;
            let mut store = self.store.lock().map_err(|_| Error::PrivateStorage)?;
            return super::device::public(&mut store, &self.config, &r);
        }
        let credential = preauth::bearer(r.header("authorization"))?;
        self.preauth
            .lock()
            .map_err(|_| Error::PrivateStorage)?
            .check(r.peer, Lane::External)?;
        let _external = slot(&self.external, 8)?;
        let cli = if r.path == "/v1/cli" && r.method == "POST" {
            require_json(&r)?;
            let mut v: super::gateway::Input = decode(&r)?;
            v.validate()?;
            if v.unsupported() {
                return Ok(Response::error(400, "unsupported_cli_mode"));
            }
            Some(v)
        } else {
            None
        };
        let (op, target) = if let Some(v) = &inbox {
            (v.operation(), None)
        } else if cli.is_some() {
            (Operation::Cli, None)
        } else {
            operation(&r.path, &r.method)?
        };
        let input: Input = if inbox.is_some() || cli.is_some() || r.body.is_empty() {
            Input::empty()
        } else {
            require_json(&r)?;
            serde_json::from_slice(&r.body).map_err(|_| Error::Invalid)?
        };
        input.validate(op)?;
        let reserved = {
            let mut store = self.store.lock().map_err(|_| Error::PrivateStorage)?;
            store.admit_from(credential, op, target, r.peer)
        };
        let reserved = match reserved {
            Ok(p) => p,
            Err(Error::Denied) => {
                let mut f = self.failures.lock().map_err(|_| Error::PrivateStorage)?;
                if f.0.elapsed() >= Duration::from_secs(1) {
                    *f = (Instant::now(), 0);
                }
                f.1 = f.1.saturating_add(1);
                return Ok(Response::error(
                    if f.1 > 16 { 429 } else { 401 },
                    "authentication_required",
                ));
            }
            Err(e) => return Err(e),
        };
        let permit = self
            .store
            .lock()
            .map_err(|_| Error::PrivateStorage)?
            .dispatch(reserved)?;
        let request_id = permit.request_id().to_owned();
        // No SQLite lock or transaction is held across Controller I/O. Exactly one call, no retries.
        let result = if let Some(input) = inbox {
            self.inbox_execute(&permit, input)
        } else if let Some(input) = cli {
            let scoped = self
                .store
                .lock()
                .map_err(|_| Error::PrivateStorage)?
                .scoped_token(&permit)?;
            if scoped && input.aggregate_mailbox() {
                super::controller::ResultValue {
                    status: 403,
                    value: json!({"error":{"code":"use_scoped_inbox"}}),
                    outcome: crate::Outcome::Unavailable,
                }
            } else {
                match &self.gateway {
                    Some(g) => g.execute(&self.config, &self.gateway_credential, input),
                    None => super::controller::ResultValue {
                        status: 503,
                        value: json!({"error":{"code":"cli_unavailable"}}),
                        outcome: crate::Outcome::Unavailable,
                    },
                }
            }
        } else {
            self.controller.execute(&permit, input)
        };
        if self
            .store
            .lock()
            .map_err(|_| Error::PrivateStorage)?
            .finish(permit, result.outcome)
            .is_err()
        {
            return Ok(Response::json(
                502,
                json!({"request_id":request_id,"error":{"code":"outcome_unknown"}}),
            ));
        }
        Ok(Response::json(
            result.status,
            json!({"request_id":request_id,"result":result.value}),
        ))
    }
    fn owner_request(&self, r: &Request) -> Result<Response, Error> {
        if r.header("authorization").is_some() {
            return Err(Error::Denied);
        }
        if r.method == "POST" {
            if !r
                .header("origin")
                .is_some_and(|o| self.config.origin_allowed(o))
                || !matches!(
                    r.header("sec-fetch-site"),
                    Some("same-origin" | "same-site")
                )
                || r.header("x-external-agent-csrf") != Some("1")
            {
                return Err(Error::Denied);
            }
            require_json(r)?;
        } else if r.header("sec-fetch-site") == Some("cross-site") {
            return Err(Error::Denied);
        }
        // Cheap rejection precedes all network/operation slots. No auth-result cache.
        let assertion = preauth::assertion(r.header("x-treer-identity-token"))?;
        let lane = match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/owner/revoke" | "/owner/activation") => Lane::Containment,
            (
                "GET",
                "/owner/state"
                | "/owner/audit"
                | "/owner/integration"
                | "/owner/external-agents"
                | "/api/tree",
            )
            | (
                "POST",
                "/owner/tokens"
                | "/owner/external-agents"
                | "/owner/external-agents/tokens"
                | "/owner/device/lookup"
                | "/owner/device/decision",
            ) => Lane::Owner,
            _ => return Ok(Response::error(404, "not_found")),
        };
        self.preauth
            .lock()
            .map_err(|_| Error::PrivateStorage)?
            .check(r.peer, lane)?;
        // Reserve two of the four management slots for revoke/activation, so
        // garbage read/create verification cannot occupy containment capacity.
        let _owner = slot(
            if lane == Lane::Containment {
                &self.containment
            } else {
                &self.owners
            },
            2,
        )?;
        // Verification happens BEFORE taking the Store mutex, with no positive cache.
        let owner = self.verifier.verify(assertion)?;
        let proof = owner.proof(&self.binding);
        let mut store = self.store.lock().map_err(|_| Error::PrivateStorage)?;
        match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/owner/device/lookup" | "/owner/device/decision") => {
                super::device::owner(&mut store, &proof, r)
            }
            ("GET", "/api/tree") => {
                let path: serde_json::Value = decode(r)?;
                let key = path["path"].as_str().ok_or(Error::Invalid)?;
                let all = store.external_agents(&proof)?;
                let agents = all["agents"].as_array().ok_or(Error::Invalid)?;
                if key.is_empty() {
                    let nodes:Vec<_>=agents.iter().filter(|a|a["enabled"]==true).map(|a|json!({"key":a["key"],"name":a["name"],"kind":"agent","capabilities":["message.send","message.read"]})).collect();
                    Ok(Response::json(
                        200,
                        json!({"schema_version":1,"path":[],"nodes":nodes}),
                    ))
                } else if agents
                    .iter()
                    .any(|a| a["key"] == key && a["enabled"] == true)
                {
                    Ok(Response::json(
                        200,
                        json!({"schema_version":1,"path":[key],"nodes":[]}),
                    ))
                } else {
                    Ok(Response::error(404, "not_found"))
                }
            }
            ("GET", "/owner/external-agents") => {
                Ok(Response::json(200, store.external_agents(&proof)?))
            }
            ("POST", "/owner/external-agents" | "/owner/external-agents/tokens") => {
                let v: super::inbox::Register = decode(r)?;
                v.validate()?;
                let c = if r.path.ends_with("/tokens") {
                    store.external_token(&proof, &v.key, &v.name, v.days)?
                } else {
                    store.register_external(&proof, &v.key, &v.name, v.days)?
                };
                let agent = store.external_agents(&proof)?["agents"]
                    .as_array()
                    .and_then(|a| a.iter().find(|a| a["key"] == v.key))
                    .cloned()
                    .ok_or(Error::Invalid)?;
                let token = serde_json::to_value(&c.record).map_err(|_| Error::Invalid)?;
                Ok(Response::json(
                    201,
                    json!({"agent":agent,"token":token,"plaintext":c.into_plaintext()}),
                ))
            }
            ("GET", "/owner/audit") => Ok(Response::json(200, store.audit_view(&proof)?)),
            ("GET", "/owner/integration") => {
                let state = store.state(&proof)?;
                Ok(Response::json(
                    200,
                    super::integration::view_at(
                        &self.config,
                        state["enabled"] == true,
                        self.advertisement.as_ref().map(|a| a.origin()),
                    ),
                ))
            }
            ("GET", "/owner/state") => {
                let mut v = store.state(&proof)?;
                v["identity"] = json!({"workspace":self.config.workspace,"app":self.config.app,"agent":self.config.agent});
                v["activation_available"] = json!(true);
                Ok(Response::json(200, v))
            }
            ("POST", "/owner/tokens") => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Create {
                    name: String,
                    days: u16,
                    authority_confirmed: bool,
                }
                let v: Create = decode(r)?;
                if !v.authority_confirmed {
                    return Err(Error::Invalid);
                }
                let c = store.create(&proof, &v.name, v.days)?;
                let record = serde_json::to_value(&c.record).map_err(|_| Error::Invalid)?;
                Ok(Response::json(
                    201,
                    json!({"token":record,"plaintext":c.into_plaintext()}),
                ))
            }
            ("POST", "/owner/revoke") => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Revoke {
                    id: String,
                }
                let v: Revoke = decode(r)?;
                store.revoke(&proof, &v.id)?;
                Ok(Response::json(200, json!({"revoked":true})))
            }
            ("POST", "/owner/activation") => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Activation {
                    enabled: bool,
                    generation: String,
                    agent: String,
                    policy_reviewed: bool,
                }
                let v: Activation = decode(r)?;
                let current = store.state(&proof)?;
                if current["generation"] != v.generation
                    || v.agent != self.config.agent
                    || v.enabled && !v.policy_reviewed
                {
                    return Err(Error::Disabled);
                }
                store.policy_reviewed(&proof, v.enabled)?;
                Ok(Response::json(200, json!({"enabled":v.enabled})))
            }
            _ => Ok(Response::error(404, "not_found")),
        }
    }
}
fn decode<T: serde::de::DeserializeOwned>(r: &Request) -> Result<T, Error> {
    serde_json::from_slice(&r.body).map_err(|_| Error::Invalid)
}
fn require_json(r: &Request) -> Result<(), Error> {
    if r.header("content-type") == Some("application/json") {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
fn operation<'a>(path: &'a str, method: &str) -> Result<(Operation, Option<&'a str>), Error> {
    if path == "/v1/agents" && method == "GET" {
        return Ok((Operation::List, None));
    }
    let s = path.strip_prefix("/v1/agents/").ok_or(Error::Invalid)?;
    let mut parts = s.split('/');
    let id = parts.next().ok_or(Error::Invalid)?;
    if !id.strip_prefix("ag_").is_some_and(|s| {
        s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err(Error::Invalid);
    }
    let suffix = parts.next();
    if parts.next().is_some() {
        return Err(Error::Invalid);
    }
    let op = match (method, suffix) {
        ("GET", None) => Operation::Status,
        ("GET", Some("output")) => Operation::Output,
        ("GET", Some("transcript")) => Operation::Transcript,
        ("POST", Some("prompt")) => Operation::Prompt,
        ("POST", Some("stop")) => Operation::Stop,
        ("POST", Some("start")) => Operation::Start,
        ("POST", Some("restart")) => Operation::Restart,
        _ => return Err(Error::Invalid),
    };
    Ok((op, Some(id)))
}
fn appearance_query(query: &str) -> bool {
    if query.is_empty() || query.len() > 2048 {
        return false;
    }
    let Ok(url) = url::Url::parse(&format!("https://invalid.example/?{query}")) else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    url.query_pairs().all(|(key, value)| {
        seen.insert(key.to_string())
            && match key.as_ref() {
                "theme" => matches!(value.as_ref(), "light" | "dark" | "system"),
                "look" => matches!(value.as_ref(), "a" | "b" | "c"),
                "lang" => matches!(value.as_ref(), "en" | "zh"),
                "embed_origin" => url::Url::parse(&value).is_ok_and(|u| {
                    matches!(u.scheme(), "http" | "https")
                        && u.host_str().is_some()
                        && u.username().is_empty()
                        && u.password().is_none()
                        && u.path() == "/"
                        && u.query().is_none()
                        && u.fragment().is_none()
                }),
                _ => false,
            }
    })
}
fn root(r: &Request, c: &Config) -> (&'static str, String) {
    // Explicit media types by q and order, then browser heuristic; neither mode carries auth data.
    let mut choices = Vec::new();
    for (i, part) in r
        .header("accept")
        .unwrap_or_default()
        .split(',')
        .enumerate()
    {
        let mut bits = part.trim().split(';');
        let kind = bits.next().unwrap_or_default().trim();
        let q = bits
            .find_map(|v| {
                v.trim()
                    .strip_prefix("q=")
                    .and_then(|v| v.parse::<f32>().ok())
            })
            .unwrap_or(1.0);
        if matches!(kind, "text/html" | "text/markdown") && q > 0.0 && q <= 1.0 {
            choices.push((q, i, kind));
        }
    }
    choices.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let html = choices
        .first()
        .map(|v| v.2 == "text/html")
        .unwrap_or_else(|| {
            r.header("user-agent")
                .is_some_and(|u| u.contains("Mozilla/"))
        });
    if html {
        (
            "text/html; charset=utf-8",
            include_str!("settings.html").replace(
                "CONSOLE_LINK",
                &c.console_url
                    .as_str()
                    .replace('&', "&amp;")
                    .replace('"', "&quot;")
                    .replace('<', "&lt;"),
            ),
        )
    } else {
        (
            "text/markdown; charset=utf-8",
            include_str!("manual.md").into(),
        )
    }
}
