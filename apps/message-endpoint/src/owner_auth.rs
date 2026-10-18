//! Current human-owner authentication for the hub settings service. No local
//! accounts, Agent management token, cached membership or delegated authority.
use crate::store::valid_id;
use reqwest::{
    blocking::{Client, Response},
    redirect::Policy,
    Url,
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, io::Read, time::Duration};
use thiserror::Error;
const LIMIT: u64 = 2 * 1024 * 1024;
type Headers<'a> = &'a [(&'a str, &'a str)];
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("owner_auth_required")]
    Required,
    #[error("owner_identity_invalid")]
    InvalidIdentity,
    #[error("current_human_owner_required")]
    OwnerRequired,
    #[error("owner_csrf_rejected")]
    Csrf,
    #[error("owner_proxy_unavailable")]
    Proxy,
    #[error("owner_auth_configuration_invalid")]
    Configuration,
    #[error("owner_oauth_state_invalid")]
    OAuthState,
    #[error("owner_login_capacity_exceeded")]
    LoginCapacity,
    #[error("owner_verification_rate_limited")]
    RateLimited,
}
pub struct Owner {
    id: String,
    expires_at: i64,
}
impl Owner {
    #[cfg(test)]
    pub(crate) fn synthetic(id: &str, expires_at: i64) -> Self {
        Self {
            id: id.into(),
            expires_at,
        }
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn expires_at(&self) -> i64 {
        self.expires_at
    }
}
pub struct OwnerAuth {
    client: Client,
    proxy: Url,
    service: String,
    workspace: String,
    origins: BTreeSet<String>,
    public_url: Url,
    gate: crate::verification_gate::Gate,
}
fn configured_url(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Configuration)?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(Error::Configuration);
    }
    Ok(url)
}
fn unique<'a>(headers: Headers<'a>, name: &str) -> Result<Option<&'a str>, Error> {
    let mut values = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v);
    let value = values.next();
    if values.next().is_some() {
        return Err(Error::InvalidIdentity);
    }
    Ok(value)
}
/// Header presence is authoritative, including empty, malformed or duplicate
/// values. An invalid supplied identity NEVER falls back to another cookie user.
fn select_token<'a>(headers: Headers<'a>, session: Option<&'a str>) -> Result<&'a str, Error> {
    let token = if let Some(token) = unique(headers, "x-treer-identity-token")? {
        Some(token)
    } else if let Some(value) = unique(headers, "authorization")? {
        Some(
            value
                .strip_prefix("Bearer ")
                .ok_or(Error::InvalidIdentity)?,
        )
    } else {
        session
    };
    let token = token.ok_or(Error::Required)?;
    if token.is_empty() || token.len() > 8192 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::InvalidIdentity);
    }
    // App identity tokens are compact JWTs. Reject malformed header floods
    // locally, but never trust decoded claims as authentication.
    let mut parts = token.split('.');
    for _ in 0..3 {
        if parts.next().is_none_or(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        }) {
            return Err(Error::InvalidIdentity);
        }
    }
    if parts.next().is_some() {
        return Err(Error::InvalidIdentity);
    }
    Ok(token)
}
impl OwnerAuth {
    pub(crate) fn settings_url(&self) -> &Url {
        &self.public_url
    }
    pub(crate) fn dedicated_settings_origin(&self) -> bool {
        self.public_url.path() == "/" && self.public_url.origin() != self.proxy.origin()
    }
    /// URLs are trusted operator configuration, never Host headers or browser
    /// parameters. HTTPS is required except literal loopback HTTP for local use.
    pub fn new(
        proxy: &str,
        service: &str,
        workspace: &str,
        public_url: &str,
    ) -> Result<Self, Error> {
        // Unlike local queue IDs, the service ID is a URL path component.
        // Never let configured separators, dot segments or percent escapes
        // change the fixed Core endpoint used by bearer authentication.
        if !valid_id(service)
            || !service.starts_with("svc_")
            || !service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || !valid_id(workspace)
        {
            return Err(Error::Configuration);
        }
        let proxy = configured_url(proxy)?;
        let public = configured_url(public_url)?;
        if proxy.path() != "/" || !public.path().ends_with('/') {
            return Err(Error::Configuration);
        }
        let origins = [public.origin().ascii_serialization()]
            .into_iter()
            .collect();
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .user_agent("Treer-Message-Hub/0.1")
            .build()
            .map_err(|_| Error::Configuration)?;
        Ok(Self {
            client,
            proxy,
            service: service.into(),
            workspace: workspace.into(),
            origins,
            public_url: public,
            gate: crate::verification_gate::Gate::default(),
        })
    }
    fn read(&self, response: Response) -> Result<Value, Error> {
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(Error::InvalidIdentity);
        }
        if !response.status().is_success() || response.content_length().is_some_and(|n| n > LIMIT) {
            return Err(Error::Proxy);
        }
        let mut bytes = vec![];
        response
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Proxy)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Proxy);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Proxy)
    }
    /// Verify every request against verify_app AND the current directory role.
    /// session_token must come only from the service's opaque OAuth session store,
    /// not browser JSON, cookies containing a raw access token, or a caller ID.
    pub fn authenticate(
        &self,
        headers: Headers<'_>,
        session_token: Option<&str>,
        now: i64,
    ) -> Result<Owner, Error> {
        // Internal callbacks without a transport source share a stricter
        // aggregate bucket. The HTTP router should use authenticate_from with
        // its actual socket peer; never accept a source address from JSON/XFF.
        self.authenticate_from(
            headers,
            session_token,
            now,
            std::net::IpAddr::from([0, 0, 0, 0]),
        )
    }
    pub fn authenticate_from(
        &self,
        headers: Headers<'_>,
        session_token: Option<&str>,
        now: i64,
        source: std::net::IpAddr,
    ) -> Result<Owner, Error> {
        if now < 0 {
            return Err(Error::InvalidIdentity);
        }
        let token = select_token(headers, session_token)?;
        let hash = crate::verification_gate::token_hash(token);
        let _permit = self.gate.enter(source, Some(hash))?;
        let result = self.verify_token(token, now);
        if let Err(error) = &result {
            self.gate.remember_failure(hash, error.clone());
        }
        result
    }
    fn verify_token(&self, token: &str, now: i64) -> Result<Owner, Error> {
        let verify = self
            .proxy
            .join(".treer/apps/identity/verify")
            .map_err(|_| Error::Configuration)?;
        let claims = self.read(
            self.client
                .post(verify)
                .json(&json!({"token":token,"audience":self.service}))
                .send()
                .map_err(|_| Error::Proxy)?,
        )?;
        let c = claims.get("claims").ok_or(Error::InvalidIdentity)?;
        let id = c
            .get("sub")
            .and_then(Value::as_str)
            .filter(|id| valid_id(id))
            .ok_or(Error::InvalidIdentity)?;
        if claims.get("active") != Some(&Value::Bool(true))
            || c.get("service_id").and_then(Value::as_str) != Some(self.service.as_str())
            || c.get("workspace_id").and_then(Value::as_str) != Some(self.workspace.as_str())
            || c.get("principal_kind").and_then(Value::as_str) != Some("human")
            || c.get("exp")
                .and_then(Value::as_i64)
                .is_none_or(|exp| exp <= now)
        {
            return Err(Error::InvalidIdentity);
        }
        let directory = self
            .proxy
            .join(&format!("api/apps/{}/directory", self.service))
            .map_err(|_| Error::Configuration)?;
        // Reqwest's bearer helper marks the Authorization header sensitive.
        let principals = self.read(
            self.client
                .get(directory)
                .bearer_auth(token)
                .send()
                .map_err(|_| Error::Proxy)?,
        )?;
        let list = principals
            .get("principals")
            .and_then(Value::as_array)
            .ok_or(Error::Proxy)?;
        let matches = list
            .iter()
            .filter(|p| {
                p.get("id").and_then(Value::as_str) == Some(id)
                    && p.get("kind").and_then(Value::as_str) == Some("human")
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 || matches[0].get("role").and_then(Value::as_str) != Some("owner") {
            return Err(Error::OwnerRequired);
        }
        Ok(Owner {
            id: id.into(),
            expires_at: c["exp"].as_i64().ok_or(Error::InvalidIdentity)?,
        })
    }
    pub(crate) fn callback_url(&self) -> Result<String, Error> {
        Ok(self
            .public_url
            .join("api/auth/callback")
            .map_err(|_| Error::Configuration)?
            .into())
    }
    pub(crate) fn authorization_url(&self, state: &str, challenge: &str) -> Result<String, Error> {
        let mut url = self
            .proxy
            .join("api/apps/oauth/authorize")
            .map_err(|_| Error::Configuration)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", self.service.as_str()),
            ("redirect_uri", self.callback_url()?.as_str()),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ]);
        Ok(url.into())
    }
    pub(crate) fn exchange_code(&self, code: &str, verifier: &str) -> Result<String, Error> {
        let _permit = self
            .gate
            .enter(std::net::IpAddr::from([0, 0, 0, 0]), None)?;
        let url = self
            .proxy
            .join("api/apps/oauth/token")
            .map_err(|_| Error::Configuration)?;
        let callback = self.callback_url()?;
        let result = self.read(
            self.client
                .post(url)
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", self.service.as_str()),
                    ("redirect_uri", callback.as_str()),
                    ("code", code),
                    ("code_verifier", verifier),
                ])
                .send()
                .map_err(|_| Error::Proxy)?,
        )?;
        if result.get("token_type").and_then(Value::as_str) != Some("Bearer")
            || result.get("audience").and_then(Value::as_str) != Some(self.service.as_str())
        {
            return Err(Error::InvalidIdentity);
        }
        result
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(Error::InvalidIdentity)
    }
    /// All management mutations are human-browser operations. Require explicit
    /// Origin even for curl, a non-simple custom header, and same-site metadata.
    /// The HTTP service must deny CORS preflights/cross-origin credentials.
    pub fn check_mutation(&self, headers: Headers<'_>) -> Result<(), Error> {
        let field = |name| unique(headers, name).map_err(|_| Error::Csrf);
        if field("x-endpoint-request")? != Some("1") {
            return Err(Error::Csrf);
        }
        if field("sec-fetch-site")?.is_some_and(|site| !matches!(site, "same-origin" | "same-site"))
        {
            return Err(Error::Csrf);
        }
        let origin = field("origin")?.ok_or(Error::Csrf)?;
        if !self.origins.contains(origin) {
            return Err(Error::Csrf);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread, time::Instant};
    const NOW: i64 = 1_800_000_000;
    const TOKEN: &str = "synthetic.header.signature";
    fn claims() -> Value {
        json!({"active":true,"claims":{"service_id":"svc_hub","workspace_id":"ws_test","sub":"human_owner","principal_kind":"human","exp":NOW+60,"role":"owner"}})
    }
    fn directory(role: &str) -> Value {
        json!({"principals":[{"kind":"human","id":"human_owner","role":role,"name":"synthetic owner","path":null}]})
    }
    fn fake(replies: Vec<Value>, token: &str) -> (OwnerAuth, thread::JoinHandle<()>) {
        fake_status(replies.into_iter().map(|v| (200, v)).collect(), token)
    }
    fn fake_status(replies: Vec<(u16, Value)>, token: &str) -> (OwnerAuth, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let auth = OwnerAuth::new(&base, "svc_hub", "ws_test", "https://hub.example/app/").unwrap();
        let token = token.to_owned();
        let join = thread::spawn(move || {
            for (status, answer) in replies {
                let began = Instant::now();
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(s) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                began.elapsed() < Duration::from_secs(2),
                                "expected authentication request"
                            );
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => panic!("private listener failed"),
                    }
                };
                // Accepted sockets inherit O_NONBLOCK on macOS. Switch the
                // private fixture connection back before read_exact; otherwise
                // a partial request intermittently fails with WouldBlock.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = vec![];
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.extend(byte);
                    assert!(request.len() < 16384);
                }
                let head = String::from_utf8(request).unwrap();
                if head.starts_with("POST ") {
                    assert!(head.starts_with("POST /.treer/apps/identity/verify HTTP/1.1"));
                    let len = head
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, n)| n.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    assert!(len < 16384);
                    let mut body = vec![0; len];
                    stream.read_exact(&mut body).unwrap();
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(value["token"], token);
                    assert_eq!(value["audience"], "svc_hub");
                } else {
                    assert!(head.starts_with("GET /api/apps/svc_hub/directory HTTP/1.1"));
                    assert!(head
                        .lines()
                        .any(|line| line.split_once(':').is_some_and(|(key, value)| key
                            .eq_ignore_ascii_case("authorization")
                            && value.trim() == format!("Bearer {token}"))));
                }
                let bytes = answer.to_string();
                use std::io::Write;
                let location = if status == 302 {
                    format!("Location: {base}redirected\r\n")
                } else {
                    String::new()
                };
                let written=write!(stream,"HTTP/1.1 {status} Test\r\n{location}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",bytes.len(),bytes);
                if bytes.len() as u64 <= LIMIT {
                    written.unwrap();
                }
                if status == 302 {
                    let at = Instant::now();
                    while at.elapsed() < Duration::from_millis(50) {
                        match listener.accept() {
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(2))
                            }
                            _ => panic!("authentication followed a redirect"),
                        }
                    }
                }
            }
            // Keep the private port owned while checking for an unexpected
            // extra verification (including negative-cache bypass). Never
            // test this by racing a closed/reusable local port.
            let at = Instant::now();
            while at.elapsed() < Duration::from_millis(50) {
                match listener.accept() {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    _ => panic!("unexpected extra authentication request"),
                }
            }
        });
        (auth, join)
    }
    fn configured() -> OwnerAuth {
        OwnerAuth::new(
            "https://proxy.example/",
            "svc_hub",
            "ws_test",
            "https://hub.example/prefix/",
        )
        .unwrap()
    }
    #[test]
    fn injected_identity_wins_and_empty_duplicate_or_invalid_never_falls_back_to_session() {
        assert_eq!(
            select_token(
                &[
                    ("X-Treer-Identity-Token", TOKEN),
                    ("Authorization", "Bearer ignored")
                ],
                Some("cookie-owner")
            )
            .unwrap(),
            TOKEN
        );
        for headers in [
            vec![("X-Treer-Identity-Token", "")],
            vec![("X-Treer-Identity-Token", "bad token")],
            vec![
                ("X-Treer-Identity-Token", TOKEN),
                ("x-treer-identity-token", TOKEN),
            ],
        ] {
            assert_eq!(
                select_token(&headers, Some(TOKEN)),
                Err(Error::InvalidIdentity)
            );
        }
        assert_eq!(
            select_token(&[("Authorization", "Basic ignored")], Some(TOKEN)),
            Err(Error::InvalidIdentity)
        );
        assert_eq!(
            select_token(
                &[
                    ("Authorization", "Bearer one"),
                    ("authorization", "Bearer two")
                ],
                Some(TOKEN)
            ),
            Err(Error::InvalidIdentity)
        );
        assert_eq!(select_token(&[], Some(TOKEN)).unwrap(), TOKEN);
        assert_eq!(select_token(&[], None), Err(Error::Required));
    }
    #[test]
    fn current_owner_is_verified_on_every_request_and_revocation_is_immediate() {
        let (auth, server) = fake(
            vec![claims(), directory("owner"), claims(), directory("member")],
            TOKEN,
        );
        assert_eq!(
            auth.authenticate(&[("X-Treer-Identity-Token", TOKEN)], None, NOW)
                .unwrap()
                .id(),
            "human_owner"
        );
        assert!(matches!(
            auth.authenticate(&[("X-Treer-Identity-Token", TOKEN)], None, NOW),
            Err(Error::OwnerRequired)
        ));
        server.join().unwrap();
    }
    #[test]
    fn forged_expired_wrong_workspace_audience_and_agent_tokens_are_rejected() {
        let mut cases = vec![];
        let mut forged = claims();
        forged["active"] = false.into();
        cases.push(forged);
        for (field, value) in [
            ("exp", json!(NOW)),
            ("exp", json!("future")),
            ("workspace_id", json!("ws_other")),
            ("service_id", json!("svc_other")),
            ("principal_kind", json!("agent")),
            ("sub", json!("")),
        ] {
            let mut invalid = claims();
            invalid["claims"][field] = value;
            cases.push(invalid);
        }
        for answer in cases {
            let (auth, server) = fake(vec![answer], "forged.header.signature");
            assert!(matches!(
                auth.authenticate(
                    &[("X-Treer-Identity-Token", "forged.header.signature")],
                    Some(TOKEN),
                    NOW
                ),
                Err(Error::InvalidIdentity)
            ));
            server.join().unwrap();
        }
    }
    #[test]
    fn absent_duplicate_or_nonhuman_directory_owner_is_denied_even_with_owner_claim() {
        for principals in [
            json!([]),
            json!([{"kind":"agent","id":"human_owner","role":"owner"}]),
            json!([{"kind":"human","id":"human_owner","role":"owner"},{"kind":"human","id":"human_owner","role":"owner"}]),
        ] {
            let (auth, server) = fake(vec![claims(), json!({"principals":principals})], TOKEN);
            assert!(matches!(
                auth.authenticate(
                    &[("Authorization", "Bearer synthetic.header.signature")],
                    None,
                    NOW
                ),
                Err(Error::OwnerRequired)
            ));
            server.join().unwrap();
        }
    }
    #[test]
    fn mutation_requires_exact_trusted_origin_custom_header_and_safe_fetch_site() {
        let auth = configured();
        assert!(auth
            .check_mutation(&[
                ("Origin", "https://hub.example"),
                ("X-Endpoint-Request", "1"),
                ("Sec-Fetch-Site", "same-origin")
            ])
            .is_ok());
        assert_eq!(
            auth.check_mutation(&[
                ("Origin", "https://proxy.example"),
                ("X-Endpoint-Request", "1"),
                ("Sec-Fetch-Site", "same-site")
            ]),
            Err(Error::Csrf)
        );
        for headers in [
            vec![("X-Endpoint-Request", "1")],
            vec![("Origin", "https://hub.example")],
            vec![
                ("Origin", "https://attacker.example"),
                ("X-Endpoint-Request", "1"),
            ],
            vec![
                ("Origin", "https://hub.example.attacker.example"),
                ("X-Endpoint-Request", "1"),
            ],
            vec![("Origin", "null"), ("X-Endpoint-Request", "1")],
            vec![
                ("Origin", "https://hub.example"),
                ("X-Endpoint-Request", "1"),
                ("Sec-Fetch-Site", "cross-site"),
            ],
            vec![
                ("Origin", "https://hub.example"),
                ("Origin", "https://hub.example"),
                ("X-Endpoint-Request", "1"),
            ],
            vec![
                ("Origin", "https://hub.example"),
                ("X-Endpoint-Request", "1"),
                ("x-endpoint-request", "1"),
            ],
        ] {
            assert_eq!(auth.check_mutation(&headers), Err(Error::Csrf));
        }
    }
    #[test]
    fn configured_origins_reject_credentials_fragments_queries_and_cleartext_nonloopback() {
        for url in [
            "http://proxy.example/",
            "file:///tmp/auth",
            "https://user:secret@proxy.example/",
            "https://proxy.example/?token=synthetic",
            "https://proxy.example/#fragment",
            "https://proxy.example:0/",
            "https://proxy.example/base/",
        ] {
            assert!(matches!(
                OwnerAuth::new(url, "svc_hub", "ws_test", "https://hub.example/"),
                Err(Error::Configuration)
            ));
        }
        assert!(OwnerAuth::new(
            "http://127.0.0.1:1234/",
            "svc_hub",
            "ws_test",
            "https://hub.example/prefix/"
        )
        .is_ok());
        assert!(OwnerAuth::new(
            "http://[::1]:1234/",
            "svc_hub",
            "ws_test",
            "https://hub.example/"
        )
        .is_ok());
        assert!(OwnerAuth::new(
            "https://proxy.example/",
            "../evil",
            "ws_test",
            "https://hub.example/"
        )
        .is_err());
    }
    #[test]
    fn proxy_redirect_is_not_followed_and_upstream_failures_are_fixed_errors() {
        for (status, error) in [
            (302, Error::Proxy),
            (401, Error::InvalidIdentity),
            (403, Error::InvalidIdentity),
            (500, Error::Proxy),
        ] {
            let (auth, server) = fake_status(
                vec![(status, json!({"provider_error":"synthetic private detail"}))],
                TOKEN,
            );
            let result = auth.authenticate(&[("X-Treer-Identity-Token", TOKEN)], None, NOW);
            assert!(matches!(result,Err(ref actual) if *actual==error));
            assert!(!format!("{error}").contains("synthetic private detail"));
            server.join().unwrap();
        }
    }
    #[test]
    fn oversized_proxy_response_and_path_component_injection_are_rejected() {
        let (auth, server) = fake(
            vec![json!("synthetic".repeat((LIMIT as usize / 9) + 1))],
            TOKEN,
        );
        assert!(matches!(
            auth.authenticate(&[("X-Treer-Identity-Token", TOKEN)], None, NOW),
            Err(Error::Proxy)
        ));
        server.join().unwrap();
        for service in [
            "svc_x/../../evil",
            "svc_%2fevil",
            "svc_x?query",
            "svc_x#fragment",
            "svc_x\\evil",
        ] {
            assert!(matches!(
                OwnerAuth::new(
                    "https://proxy.example/",
                    service,
                    "ws_test",
                    "https://hub.example/"
                ),
                Err(Error::Configuration)
            ));
        }
    }
    #[test]
    fn only_the_configured_settings_origin_is_allowed_even_for_proxy_tunnels() {
        let auth = OwnerAuth::new(
            "https://proxy.example/",
            "svc_hub",
            "ws_test",
            "https://proxy.example/api/workspaces/ws_test/apps/svc_hub/ui/",
        )
        .unwrap();
        assert!(auth
            .check_mutation(&[
                ("Origin", "https://proxy.example"),
                ("X-Endpoint-Request", "1")
            ])
            .is_ok());
        assert_eq!(
            auth.check_mutation(&[
                ("Origin", "https://hub.example"),
                ("X-Endpoint-Request", "1")
            ]),
            Err(Error::Csrf)
        );
        assert_eq!(
            configured().check_mutation(&[
                ("Origin", "https://proxy.example"),
                ("X-Endpoint-Request", "1")
            ]),
            Err(Error::Csrf)
        );
    }
    #[test]
    fn cheap_token_shape_checks_and_concurrency_refusal_happen_before_any_proxy_call() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let auth = OwnerAuth::new(
            &format!("http://{}/", listener.local_addr().unwrap()),
            "svc_hub",
            "ws_test",
            "https://hub.example/",
        )
        .unwrap();
        for token in [
            "not-jwt",
            "a..c",
            "a.b.c.extra",
            "a.b.c=",
            "a.b.c/",
            "a.b.c\n",
        ] {
            assert!(matches!(
                auth.authenticate(&[("X-Treer-Identity-Token", token)], None, NOW),
                Err(Error::InvalidIdentity)
            ));
        }
        let source = std::net::IpAddr::from([127, 0, 0, 1]);
        let a = auth.gate.enter(source, None).unwrap();
        let b = auth.gate.enter(source, None).unwrap();
        // Admission must return before even connecting to the owned listener.
        assert!(matches!(
            auth.authenticate_from(&[("X-Treer-Identity-Token", TOKEN)], None, NOW, source),
            Err(Error::RateLimited)
        ));
        drop((a, b));
        assert!(matches!(listener.accept(),Err(ref e) if e.kind()==std::io::ErrorKind::WouldBlock));
    }
    #[test]
    fn negative_only_cache_skips_repeat_failed_token_verification_without_caching_positive_membership(
    ) {
        let mut inactive = claims();
        inactive["active"] = false.into();
        let (auth, server) = fake(vec![inactive], TOKEN);
        assert!(matches!(
            auth.authenticate(&[("X-Treer-Identity-Token", TOKEN)], None, NOW),
            Err(Error::InvalidIdentity)
        ));
        // The private listener remains owned and rejects extra requests. This
        // asserts the failure-only cache, while the positive revocation test
        // above still makes four calls.
        assert!(matches!(
            auth.authenticate(&[("X-Treer-Identity-Token", TOKEN)], None, NOW),
            Err(Error::InvalidIdentity)
        ));
        server.join().unwrap();
    }
}
