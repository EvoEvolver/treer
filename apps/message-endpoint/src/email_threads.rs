//! Scoped member thread HTTP. Separate cookies, OAuth flows and verified Member
//! capabilities; management still requires Owner. Not assembled by default.
use crate::settings_http::{callback, cookie, error, fixed, redirect, response, secure};
use crate::{
    email::{self, Thread},
    oauth::MemberOAuthVault,
    owner_auth::{Error, Member, MemberAuth},
};
use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;
const SESSION: &str = "__Host-treer-endpoint-member-session";
const BROWSER: &str = "__Host-treer-endpoint-member-login";
type Read = dyn Fn(&Member, &str, i64) -> Result<Thread, email::Error> + Send + Sync;
type Reply = dyn Fn(&Member, &str, &str, &str, i64) -> Result<i64, email::Error> + Send + Sync;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission {
    key: String,
    body: String,
}
pub struct Service {
    auth: MemberAuth,
    vault: MemberOAuthVault,
    public_url: reqwest::Url,
    destinations: Mutex<HashMap<[u8; 32], (String, i64)>>,
    slots: Arc<Semaphore>,
    read: Arc<Read>,
    reply: Arc<Reply>,
}
fn clock() -> Result<i64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|t| i64::try_from(t.as_secs()).ok())
        .ok_or(Error::Proxy)
}
fn thread_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
fn query_thread(query: Option<&str>) -> Result<String, Error> {
    let query = query.ok_or(Error::OAuthState)?;
    if query.len() > 128 {
        return Err(Error::OAuthState);
    }
    let url = reqwest::Url::parse(&format!("https://invalid.example/?{query}"))
        .map_err(|_| Error::OAuthState)?;
    let values = url.query_pairs().collect::<Vec<_>>();
    if values.len() != 1 || values[0].0 != "thread" || !thread_id(&values[0].1) {
        return Err(Error::OAuthState);
    }
    Ok(values[0].1.to_string())
}
fn tokens(headers: &HeaderMap) -> Result<(Option<String>, Option<String>), Error> {
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
                session = Some(value.into());
            }
            if name == BROWSER {
                browser = Some(value.into());
            }
        }
    }
    Ok((session, browser))
}
fn queue_error(e: email::Error) -> Response {
    let code = match &e {
        email::Error::Denied => StatusCode::FORBIDDEN,
        email::Error::Invalid => StatusCode::BAD_REQUEST,
        email::Error::Conflict | email::Error::Lease => StatusCode::CONFLICT,
        email::Error::Rate => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    fixed(code, &e.to_string())
}
impl Service {
    pub fn new(
        auth: MemberAuth,
        public_url: &str,
        read: Arc<Read>,
        reply: Arc<Reply>,
    ) -> Result<Self, Error> {
        let url = reqwest::Url::parse(public_url).map_err(|_| Error::Configuration)?;
        // The verified OAuth client and response links must name the same
        // configured dedicated origin; never infer it from Host/forwarded data.
        let client = auth.oauth_client();
        if !client.dedicated_settings_origin() || url != *client.settings_url() {
            return Err(Error::Configuration);
        }
        Ok(Self {
            auth,
            vault: MemberOAuthVault::default(),
            public_url: url,
            destinations: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(4)),
            read,
            reply,
        })
    }
    fn member(&self, headers: &HeaderMap, peer: SocketAddr, now: i64) -> Result<Member, Error> {
        let values = headers
            .iter()
            .map(|(name, value)| value.to_str().map(|value| (name.as_str(), value)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::InvalidIdentity)?;
        let (session, _) = tokens(headers)?;
        let token = session
            .as_deref()
            .map(|value| self.vault.session_token(value, now))
            .transpose()?
            .flatten();
        self.auth
            .authenticate_from(&values, token.as_deref(), now, peer.ip())
    }
    fn mutation(&self, headers: &HeaderMap) -> Result<(), Error> {
        let values = headers
            .iter()
            .map(|(name, value)| value.to_str().map(|value| (name.as_str(), value)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::InvalidIdentity)?;
        self.auth.check_mutation(&values)
    }
    fn destination(&self, id: &str) -> String {
        let mut url = self.public_url.clone();
        url.set_fragment(Some(&format!("email-thread={id}")));
        url.into()
    }
    fn start(
        &self,
        headers: &HeaderMap,
        peer: SocketAddr,
        query: Option<&str>,
        now: i64,
    ) -> Response {
        let id = match query_thread(query) {
            Ok(id) => id,
            Err(e) => return error(e),
        };
        match self.member(headers, peer, now) {
            Ok(_) => return redirect(&self.destination(&id)),
            Err(Error::Required) => {}
            Err(e) => return error(e),
        }
        let mut destinations = match self.destinations.lock() {
            Ok(v) => v,
            Err(_) => return error(Error::Proxy),
        };
        destinations.retain(|_, (_, expires)| *expires > now);
        if destinations.len() >= 1000 {
            return error(Error::LoginCapacity);
        }
        match self.vault.start_from(&self.auth, now, peer.ip()) {
            Ok(start) => {
                let state = reqwest::Url::parse(&start.location).ok().and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.into_owned())
                });
                let Some(state) = state else {
                    return error(Error::Configuration);
                };
                let Some(expires) = now.checked_add(300) else {
                    return error(Error::OAuthState);
                };
                destinations.insert(Sha256::digest(state.as_bytes()).into(), (id, expires));
                let mut result = redirect(&start.location);
                cookie(&mut result, BROWSER, start.browser_secret(), 300);
                result
            }
            Err(e) => error(e),
        }
    }
    fn callback(&self, headers: &HeaderMap, query: Option<&str>, now: i64) -> Response {
        let (code, state) = match callback(query.unwrap_or("")) {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        let (_, browser) = match tokens(headers) {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        let key: [u8; 32] = Sha256::digest(state.as_bytes()).into();
        match self.vault.finish(
            &self.auth,
            &code,
            &state,
            browser.as_deref().unwrap_or(""),
            now,
        ) {
            Ok(login) => {
                let target = self
                    .destinations
                    .lock()
                    .ok()
                    .and_then(|mut m| m.remove(&key))
                    .filter(|(_, expiry)| *expiry > now)
                    .map(|(id, _)| id);
                let Some(id) = target else {
                    let _ = self.vault.logout(login.cookie_secret());
                    let mut result = error(Error::OAuthState);
                    cookie(&mut result, BROWSER, "", 0);
                    return result;
                };
                let mut result = redirect(&self.destination(&id));
                cookie(&mut result, BROWSER, "", 0);
                cookie(
                    &mut result,
                    SESSION,
                    login.cookie_secret(),
                    login.expires_at().saturating_sub(now),
                );
                result
            }
            Err(failure) => {
                let mut result = error(failure.error);
                if failure.clear_browser_cookie {
                    if let Ok(mut destinations) = self.destinations.lock() {
                        destinations.remove(&key);
                    }
                    cookie(&mut result, BROWSER, "", 0);
                }
                result
            }
        }
    }
    fn dispatch_read(
        &self,
        method: &Method,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        peer: SocketAddr,
        now: i64,
    ) -> Response {
        if method == Method::GET && path == "/api/email/auth/start" {
            return self.start(headers, peer, query, now);
        }
        if method == Method::GET && path == "/api/email/auth/callback" {
            return self.callback(headers, query, now);
        }
        if query.is_some() {
            return fixed(StatusCode::BAD_REQUEST, "query_not_allowed");
        }
        if method == Method::POST && path == "/api/email/auth/logout" {
            if let Err(e) = self.mutation(headers) {
                return error(e);
            }
            let (session, _) = match tokens(headers) {
                Ok(v) => v,
                Err(e) => return error(e),
            };
            if let Some(session) = session {
                if let Err(e) = self.vault.logout(&session) {
                    return error(e);
                }
            }
            let mut result = response(StatusCode::OK, json!({"logged_out":true}));
            cookie(&mut result, SESSION, "", 0);
            return result;
        }
        let Some(id) = path
            .strip_prefix("/api/email/thread/")
            .filter(|id| thread_id(id))
        else {
            return fixed(StatusCode::NOT_FOUND, "route_not_found");
        };
        if method != Method::GET {
            return fixed(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
        }
        let member = match self.member(headers, peer, now) {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        match (self.read)(&member, id, now) {
            Ok(thread) => response(
                StatusCode::OK,
                json!({"member_id":member.id(),"thread":thread}),
            ),
            Err(e) => queue_error(e),
        }
    }
    /// Parent Settings applies total header bounds and trusted Host matching.
    pub(crate) async fn handle(
        self: Arc<Self>,
        peer: SocketAddr,
        parts: axum::http::request::Parts,
        body: Body,
        allowed_hosts: &BTreeSet<String>,
    ) -> Response {
        let mut hosts = parts.headers.get_all("host").iter();
        if !hosts
            .next()
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| allowed_hosts.contains(&v.to_ascii_lowercase()))
            || hosts.next().is_some()
        {
            return secure(error(Error::Csrf));
        }
        let permit = match self.slots.clone().try_acquire_owned() {
            Ok(v) => v,
            Err(_) => return secure(error(Error::RateLimited)),
        };
        let id = parts
            .uri
            .path()
            .strip_prefix("/api/email/thread/")
            .filter(|id| thread_id(id))
            .map(str::to_owned);
        if parts.method == Method::POST && id.is_some() {
            if parts.uri.query().is_some() {
                return secure(fixed(StatusCode::BAD_REQUEST, "query_not_allowed"));
            }
            let lengths = parts
                .headers
                .get_all("content-length")
                .iter()
                .collect::<Vec<_>>();
            let size = lengths
                .first()
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<usize>().ok());
            let types = parts
                .headers
                .get_all("content-type")
                .iter()
                .collect::<Vec<_>>();
            if parts.headers.contains_key("transfer-encoding")
                || lengths.len() != 1
                || !size.is_some_and(|n| n > 0 && n <= 16384)
                || types.len() != 1
                || types[0].as_bytes() != b"application/json"
            {
                return secure(fixed(StatusCode::BAD_REQUEST, "invalid_email_body"));
            }
            let app = self.clone();
            let verified = tokio::task::spawn_blocking(move || {
                let now = clock()?;
                app.mutation(&parts.headers)?;
                let member = app.member(&parts.headers, peer, now)?;
                Ok::<_, Error>((member, permit))
            })
            .await;
            let (member, permit) = match verified {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return secure(error(e)),
                Err(_) => return secure(error(Error::Proxy)),
            };
            let bytes =
                match tokio::time::timeout(Duration::from_secs(3), to_bytes(body, 16384)).await {
                    Ok(Ok(b)) if Some(b.len()) == size => b,
                    _ => return secure(fixed(StatusCode::BAD_REQUEST, "invalid_email_body")),
                };
            return match tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let request = match serde_json::from_slice::<Submission>(&bytes) {
                    Ok(v) => v,
                    Err(_) => return secure(fixed(StatusCode::BAD_REQUEST, "invalid_email_body")),
                };
                let now = match clock() {
                    Ok(v) => v,
                    Err(e) => return secure(error(e)),
                };
                match (self.reply)(
                    &member,
                    id.as_deref().unwrap_or(""),
                    &request.key,
                    &request.body,
                    now,
                ) {
                    Ok(_) => secure(response(StatusCode::ACCEPTED, json!({"status":"queued"}))),
                    Err(e) => secure(queue_error(e)),
                }
            })
            .await
            {
                Ok(r) => r,
                Err(_) => secure(error(Error::Proxy)),
            };
        }
        if parts.headers.contains_key("transfer-encoding")
            || parts
                .headers
                .get_all("content-length")
                .iter()
                .any(|v| v.as_bytes() != b"0")
        {
            return secure(fixed(StatusCode::BAD_REQUEST, "request_body_not_allowed"));
        }
        match tokio::time::timeout(Duration::from_secs(3), to_bytes(body, 1024)).await {
            Ok(Ok(b)) if b.is_empty() => {}
            _ => return secure(fixed(StatusCode::BAD_REQUEST, "request_body_not_allowed")),
        }
        match tokio::task::spawn_blocking(move || {
            let _permit = permit;
            match clock() {
                Ok(now) => secure(self.dispatch_read(
                    &parts.method,
                    parts.uri.path(),
                    parts.uri.query(),
                    &parts.headers,
                    peer,
                    now,
                )),
                Err(e) => secure(error(e)),
            }
        })
        .await
        {
            Ok(r) => r,
            Err(_) => secure(error(Error::Proxy)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        email::{Contact, Queue},
        protocol::{Envelope, Sender, SenderKind},
        store::{Limits, Store},
    };
    use std::{
        io::{Read as IoRead, Write},
        net::TcpListener,
        sync::atomic::{AtomicBool, Ordering},
        thread,
    };
    struct Proxy {
        url: String,
        revoked: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }
    impl Proxy {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let revoked = Arc::new(AtomicBool::new(false));
            let (stopping, removed) = (stop.clone(), revoked.clone());
            let join = thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(v) => v,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(_) => break,
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut head = Vec::new();
                    let mut byte = [0; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        stream.read_exact(&mut byte).unwrap();
                        head.extend(byte);
                        assert!(head.len() < 16384);
                    }
                    let head = String::from_utf8(head).unwrap();
                    let size = head
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
                    let value = if head.starts_with("POST /api/apps/oauth/token ") {
                        json!({"access_token":"synthetic.alice.signature","token_type":"Bearer","audience":"svc_hub"})
                    } else if head.starts_with("POST /.treer/apps/identity/verify ") {
                        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(request["audience"], "svc_hub");
                        let token = request["token"].as_str().unwrap_or("");
                        let human = match token {
                            "synthetic.alice.signature" => Some("user_alice"),
                            "synthetic.bob.signature" => Some("user_bob"),
                            _ => None,
                        };
                        match human {
                            Some(id) => {
                                json!({"active":true,"claims":{"sub":id,"principal_kind":"human","service_id":"svc_hub","workspace_id":"ws_test","exp":clock().unwrap()+300}})
                            }
                            None => json!({"active":false}),
                        }
                    } else {
                        assert!(head.starts_with("GET /api/apps/svc_hub/directory "));
                        json!({"principals":if removed.load(Ordering::Acquire){vec![json!({"id":"user_bob","kind":"human","role":"member"})]}else{vec![json!({"id":"user_alice","kind":"human","role":"member"}),json!({"id":"user_bob","kind":"human","role":"member"})]}})
                    };
                    let value = value.to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        value.len(),
                        value
                    )
                    .unwrap();
                }
            });
            Self {
                url,
                revoked,
                stop,
                join: Some(join),
            }
        }
    }
    impl Drop for Proxy {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(join) = self.join.take() {
                join.join().unwrap();
            }
        }
    }
    #[test]
    fn thread_queries_and_cookie_scopes_cannot_be_redirects_or_owner_sessions() {
        let id = "a".repeat(64);
        assert_eq!(query_thread(Some(&format!("thread={id}"))).unwrap(), id);
        for query in [
            "thread=https://evil.test/",
            "thread=short",
            "thread=a&thread=b",
            "thread=a&next=evil",
        ] {
            assert!(query_thread(Some(query)).is_err());
        }
        let headers = HeaderMap::from_iter([(
            axum::http::header::COOKIE,
            axum::http::HeaderValue::from_static("__Host-treer-endpoint-session=owner-secret"),
        )]);
        assert_eq!(tokens(&headers).unwrap(), (None, None));
    }
    #[test]
    fn member_http_enforces_binding_csrf_current_membership_and_strict_body() {
        let proxy = Proxy::start();
        let mut store = Store::memory(Limits::default()).unwrap();
        store.register_paused("ag_email", 1, "ag_internal").unwrap();
        Queue::new(&mut store)
            .unwrap()
            .bind_paused(
                "ag_email",
                1,
                Contact {
                    address: "person@example.test".into(),
                    member_id: "user_alice".into(),
                },
            )
            .unwrap();
        store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        let mut queue = Queue::new(&mut store).unwrap();
        let id = queue
            .admit(
                "ag_email",
                1,
                &Envelope::Outbound {
                    delivery_id: "delivery_one".into(),
                    message_id: "msg_one".into(),
                    original_sender: Sender {
                        kind: SenderKind::Agent,
                        id: "ag_internal".into(),
                    },
                    body: "<script>private</script>".into(),
                    context_ids: vec![],
                },
                clock().unwrap(),
            )
            .unwrap();
        let state = Arc::new(Mutex::new(store));
        let read = state.clone();
        let reply = state.clone();
        let auth =
            MemberAuth::new(&proxy.url, "svc_hub", "ws_test", "https://hub.example/").unwrap();
        let service = Arc::new(
            Service::new(
                auth,
                "https://hub.example/",
                Arc::new(move |member, id, now| {
                    Queue::new(&mut read.lock().unwrap())
                        .unwrap()
                        .thread(id, member, now)
                }),
                Arc::new(move |member, id, key, body, now| {
                    Queue::new(&mut reply.lock().unwrap())
                        .unwrap()
                        .reply(id, member, key, body, now)
                }),
            )
            .unwrap(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let hosts = BTreeSet::from(["hub.example".to_owned()]);
        let path = format!("/api/email/thread/{id}");
        let request =
            |method: Method, token: Option<&str>, origin: Option<&str>, body: Option<&str>| {
                let mut request = axum::http::Request::builder()
                    .uri(&path)
                    .method(method)
                    .header("Host", "hub.example");
                if let Some(token) = token {
                    request = request.header("X-Treer-Identity-Token", token);
                }
                if let Some(origin) = origin {
                    request = request
                        .header("Origin", origin)
                        .header("X-Endpoint-Request", "1")
                        .header("Sec-Fetch-Site", "same-origin");
                }
                if let Some(body) = body {
                    request = request
                        .header("Content-Type", "application/json")
                        .header("Content-Length", body.len().to_string());
                }
                let (parts, body) = request
                    .body(Body::from(body.unwrap_or("").to_owned()))
                    .unwrap()
                    .into_parts();
                runtime.block_on(service.clone().handle(
                    "127.0.0.1:1234".parse().unwrap(),
                    parts,
                    body,
                    &hosts,
                ))
            };
        assert_eq!(
            request(Method::GET, None, None, None).status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(Method::GET, Some("synthetic.forged.signature"), None, None).status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(Method::GET, Some("synthetic.bob.signature"), None, None).status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(Method::GET, Some("synthetic.alice.signature"), None, None).status(),
            StatusCode::OK
        );
        let body = r#"{"key":"one","body":"member reply"}"#;
        assert_eq!(
            request(
                Method::POST,
                Some("synthetic.alice.signature"),
                None,
                Some(body)
            )
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                Method::POST,
                Some("synthetic.alice.signature"),
                Some("https://evil.example"),
                Some(body)
            )
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                Method::POST,
                Some("synthetic.bob.signature"),
                Some("https://hub.example"),
                Some(body)
            )
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                Method::POST,
                Some("synthetic.alice.signature"),
                Some("https://hub.example"),
                Some(r#"{"key":"one","body":"reply","recipient":"ag_other"}"#)
            )
            .status(),
            StatusCode::BAD_REQUEST
        );
        for _ in 0..2 {
            assert_eq!(
                request(
                    Method::POST,
                    Some("synthetic.alice.signature"),
                    Some("https://hub.example"),
                    Some(body)
                )
                .status(),
                StatusCode::ACCEPTED
            );
        }
        let n: i64 = state
            .lock()
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM inbound", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        proxy.revoked.store(true, Ordering::Release);
        assert_eq!(
            request(Method::GET, Some("synthetic.alice.signature"), None, None).status(),
            StatusCode::FORBIDDEN
        );
    }
    #[test]
    fn member_oauth_http_has_fixed_return_and_invalid_injected_never_falls_back() {
        let proxy = Proxy::start();
        let service = Arc::new(
            Service::new(
                MemberAuth::new(&proxy.url, "svc_hub", "ws_test", "https://hub.example/").unwrap(),
                "https://hub.example/",
                Arc::new(|_, _, _| Err(email::Error::Denied)),
                Arc::new(|_, _, _, _, _| Err(email::Error::Denied)),
            )
            .unwrap(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let hosts = BTreeSet::from(["hub.example".to_owned()]);
        let call = |path: &str, browser: Option<&str>, injected: Option<&str>| {
            let mut r = axum::http::Request::builder()
                .uri(path)
                .header("Host", "hub.example");
            if let Some(browser) = browser {
                r = r.header("Cookie", browser);
            }
            if let Some(injected) = injected {
                r = r.header("X-Treer-Identity-Token", injected);
            }
            let (parts, body) = r.body(Body::empty()).unwrap().into_parts();
            runtime.block_on(service.clone().handle(
                "127.0.0.1:1234".parse().unwrap(),
                parts,
                body,
                &hosts,
            ))
        };
        let id = "b".repeat(64);
        let start = call(&format!("/api/email/auth/start?thread={id}"), None, None);
        assert_eq!(start.status(), StatusCode::SEE_OTHER);
        let location = reqwest::Url::parse(start.headers()["location"].to_str().unwrap()).unwrap();
        let state = location
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        assert_eq!(
            location
                .query_pairs()
                .find(|(k, _)| k == "redirect_uri")
                .unwrap()
                .1,
            "https://hub.example/api/email/auth/callback"
        );
        let browser = start.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let path = format!("/api/email/auth/callback?code=synthetic-code&state={state}");
        assert_eq!(
            call(
                &path,
                Some("__Host-treer-endpoint-member-login=wrong"),
                None
            )
            .status(),
            StatusCode::FORBIDDEN
        );
        let callback = call(&path, Some(browser), None);
        assert_eq!(callback.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            callback.headers()["location"],
            format!("https://hub.example/#email-thread={id}")
        );
        let session = callback
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|h| h.to_str().ok())
            .find(|h| h.starts_with(SESSION))
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert!(!session.contains("synthetic.alice"));
        let thread = format!("/api/email/thread/{id}");
        // The handler reached its scoped read callback, not a login error.
        assert_eq!(
            call(&thread, Some(session), None).status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&thread, Some(session), Some("synthetic.forged.signature")).status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&path, Some(browser), None).status(),
            StatusCode::FORBIDDEN
        );
    }
}
