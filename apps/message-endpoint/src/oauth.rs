//! Bounded, in-memory standard App OAuth state/PKCE and opaque sessions. No
//! forwarded identity token is persisted. Restart deliberately signs sessions out.
use crate::owner_auth::{Error, OwnerAuth};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Mutex};
const FLOWS: usize = 100;
const SESSIONS: usize = 100;
const FLOW_TTL: i64 = 600;
const SESSION_TTL: i64 = 12 * 3600;
struct Pending {
    verifier: String,
    browser: [u8; 32],
    created_at: i64,
    expires: i64,
}
struct Session {
    token: String,
    created_at: i64,
    expires: i64,
}
#[derive(Default)]
struct Memory {
    flows: HashMap<[u8; 32], Pending>,
    sessions: HashMap<[u8; 32], Session>,
}
#[derive(Default)]
pub struct OAuthVault {
    state: Mutex<Memory>,
}
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct CallbackError {
    pub error: Error,
    /// A mismatched browser/state must not clear another valid flow's cookie.
    pub clear_browser_cookie: bool,
}
/// Router sends only the URL as Location and the browser secret as an HttpOnly
/// nonce cookie, never a JSON response, URL parameter, log or HTML field.
pub struct LoginStart {
    pub location: String,
    browser_secret: String,
}
impl LoginStart {
    pub fn browser_secret(&self) -> &str {
        &self.browser_secret
    }
}
pub struct LoginSession {
    secret: String,
    expires: i64,
}
impl LoginSession {
    pub fn cookie_secret(&self) -> &str {
        &self.secret
    }
    pub fn expires_at(&self) -> i64 {
        self.expires
    }
}
fn hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}
fn random(bytes: usize) -> Result<String, Error> {
    let mut value = vec![0; bytes];
    getrandom::fill(&mut value).map_err(|_| Error::Proxy)?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}
fn opaque(value: &str) -> bool {
    (32..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
impl Memory {
    fn cleanup(&mut self, now: i64) {
        self.flows.retain(|_, flow| flow.expires > now);
        self.sessions.retain(|_, session| session.expires > now);
    }
}
impl OAuthVault {
    pub fn start(&self, auth: &OwnerAuth, now: i64) -> Result<LoginStart, Error> {
        if now < 0 {
            return Err(Error::OAuthState);
        }
        let expires = now.checked_add(FLOW_TTL).ok_or(Error::OAuthState)?;
        let state = random(32)?;
        let browser_secret = random(32)?;
        let verifier = random(48)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let location = auth.authorization_url(&state, &challenge)?;
        let mut memory = self.state.lock().map_err(|_| Error::Proxy)?;
        memory.cleanup(now);
        if memory.flows.len() >= FLOWS {
            return Err(Error::LoginCapacity);
        }
        if memory.flows.contains_key(&hash(&state)) {
            return Err(Error::Proxy);
        }
        memory.flows.insert(
            hash(&state),
            Pending {
                verifier,
                browser: hash(&browser_secret),
                created_at: now,
                expires,
            },
        );
        Ok(LoginStart {
            location,
            browser_secret,
        })
    }
    fn consume(&self, state: &str, browser: &str, now: i64) -> Result<Pending, CallbackError> {
        let unmatched = |error| CallbackError {
            error,
            clear_browser_cookie: false,
        };
        let consumed_error = |error| CallbackError {
            error,
            clear_browser_cookie: true,
        };
        if !opaque(state) || !opaque(browser) || now < 0 {
            return Err(unmatched(Error::OAuthState));
        }
        let mut memory = self.state.lock().map_err(|_| unmatched(Error::Proxy))?;
        let key = hash(state);
        // A mismatch does not destroy another browser's legitimate flow.
        let matched = memory
            .flows
            .get(&key)
            .is_some_and(|flow| flow.browser == hash(browser));
        if !matched {
            memory.cleanup(now);
            return Err(unmatched(Error::OAuthState));
        }
        let flow = memory
            .flows
            .remove(&key)
            .ok_or_else(|| consumed_error(Error::OAuthState))?;
        memory.cleanup(now);
        if flow.expires <= now || now < flow.created_at {
            return Err(consumed_error(Error::OAuthState));
        }
        Ok(flow)
    }
    /// Consume matching state before token exchange (even on failure); only the
    /// current verified human owner gets a session. Network calls hold no vault
    /// or SQLite lock. Access tokens stay solely in this bounded process memory.
    pub fn finish(
        &self,
        auth: &OwnerAuth,
        code: &str,
        state: &str,
        browser: &str,
        now: i64,
    ) -> Result<LoginSession, CallbackError> {
        let flow = self.consume(state, browser, now)?;
        let complete = || -> Result<LoginSession, Error> {
            if code.is_empty() || code.len() > 1024 || !code.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(Error::OAuthState);
            }
            let token = auth.exchange_code(code, &flow.verifier)?;
            let owner = auth.authenticate(&[], Some(&token), now)?;
            let expires = owner
                .expires_at()
                .min(now.checked_add(SESSION_TTL).ok_or(Error::OAuthState)?);
            let secret = random(48)?;
            let mut memory = self.state.lock().map_err(|_| Error::Proxy)?;
            memory.cleanup(now);
            if memory.sessions.len() >= SESSIONS {
                return Err(Error::LoginCapacity);
            }
            if memory.sessions.contains_key(&hash(&secret)) {
                return Err(Error::Proxy);
            }
            memory.sessions.insert(
                hash(&secret),
                Session {
                    token,
                    created_at: now,
                    expires,
                },
            );
            Ok(LoginSession { secret, expires })
        };
        complete().map_err(|error| CallbackError {
            error,
            clear_browser_cookie: true,
        })
    }
    /// Only the trusted router may obtain this token for per-request verification;
    /// never serialize it. Cookie input is an opaque random session secret.
    pub fn session_token(&self, raw: &str, now: i64) -> Result<Option<String>, Error> {
        if !opaque(raw) || now < 0 {
            return Ok(None);
        }
        let mut memory = self.state.lock().map_err(|_| Error::Proxy)?;
        memory.cleanup(now);
        Ok(memory
            .sessions
            .get(&hash(raw))
            .filter(|session| now >= session.created_at)
            .map(|session| session.token.clone()))
    }
    pub fn logout(&self, raw: &str) -> Result<(), Error> {
        if !opaque(raw) {
            return Ok(());
        }
        self.state
            .lock()
            .map_err(|_| Error::Proxy)?
            .sessions
            .remove(&hash(raw));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Url;
    use serde_json::{json, Value};
    use std::{
        collections::HashSet,
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };
    const NOW: i64 = 1_800_000_000;
    fn configured() -> OwnerAuth {
        OwnerAuth::new(
            "https://proxy.example/",
            "svc_hub",
            "ws_test",
            "https://hub.example/prefix/",
        )
        .unwrap()
    }
    fn query(start: &LoginStart) -> HashMap<String, String> {
        Url::parse(&start.location)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }
    #[test]
    fn authorization_url_is_fixed_pkce_and_nonce_is_private_not_a_query_parameter() {
        let vault = OAuthVault::default();
        let auth = configured();
        let start = vault.start(&auth, NOW).unwrap();
        let q = query(&start);
        assert_eq!(q["client_id"], "svc_hub");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(
            q["redirect_uri"],
            "https://hub.example/prefix/api/auth/callback"
        );
        assert!(start
            .location
            .starts_with("https://proxy.example/api/apps/oauth/authorize?"));
        assert!(!start.location.contains(start.browser_secret()));
        assert!(!q.contains_key("code_verifier"));
        assert!(opaque(&q["state"]));
        assert!(opaque(start.browser_secret()));
        let memory = vault.state.lock().unwrap();
        let flow = &memory.flows[&hash(&q["state"])];
        assert_eq!(flow.verifier.len(), 64);
        assert_eq!(
            q["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(flow.verifier.as_bytes()))
        );
        assert!(!start.location.contains(&flow.verifier));
        assert_eq!(flow.expires, NOW + 600);
    }
    #[test]
    fn state_is_browser_bound_single_use_and_expiry_does_not_clear_unmatched_cookie() {
        let vault = OAuthVault::default();
        let auth = configured();
        let start = vault.start(&auth, NOW).unwrap();
        let state = query(&start)["state"].clone();
        let wrong = vault.consume(&state, &"z".repeat(43), NOW).err().unwrap();
        assert!(!wrong.clear_browser_cookie);
        assert_eq!(vault.state.lock().unwrap().flows.len(), 1);
        assert!(vault.consume(&state, start.browser_secret(), NOW).is_ok());
        assert!(vault.consume(&state, start.browser_secret(), NOW).is_err());
        let expired = vault.start(&auth, NOW).unwrap();
        let state = query(&expired)["state"].clone();
        let result = vault
            .consume(&state, expired.browser_secret(), NOW + 600)
            .err()
            .unwrap();
        assert!(result.clear_browser_cookie);
        assert_eq!(result.error, Error::OAuthState);
        assert!(vault.state.lock().unwrap().flows.is_empty());
    }
    #[test]
    fn parallel_flow_mismatch_cannot_consume_other_flow_or_extend_it_on_clock_rollback() {
        let vault = OAuthVault::default();
        let auth = configured();
        let a = vault.start(&auth, NOW).unwrap();
        let b = vault.start(&auth, NOW).unwrap();
        let qa = query(&a);
        let qb = query(&b);
        assert!(vault
            .consume(&qa["state"], b.browser_secret(), NOW)
            .is_err());
        assert!(vault
            .consume(&qb["state"], a.browser_secret(), NOW)
            .is_err());
        assert_eq!(vault.state.lock().unwrap().flows.len(), 2);
        assert!(vault.consume(&qa["state"], a.browser_secret(), NOW).is_ok());
        assert!(vault
            .consume(&qb["state"], b.browser_secret(), NOW - 1)
            .is_err());
        assert!(vault.state.lock().unwrap().flows.is_empty());
    }
    #[test]
    fn flows_are_bounded_random_and_expired_slots_can_be_reused_without_eviction() {
        let vault = OAuthVault::default();
        let auth = configured();
        let mut states = HashSet::new();
        let mut browsers = HashSet::new();
        for _ in 0..FLOWS {
            let start = vault.start(&auth, NOW).unwrap();
            assert!(states.insert(query(&start)["state"].clone()));
            assert!(browsers.insert(start.browser_secret().to_owned()));
        }
        assert!(matches!(vault.start(&auth, NOW), Err(Error::LoginCapacity)));
        assert_eq!(vault.state.lock().unwrap().flows.len(), 100);
        assert!(vault.start(&auth, NOW + 600).is_ok());
        assert_eq!(vault.state.lock().unwrap().flows.len(), 1);
    }
    fn fake_proxy(role: &str) -> (OwnerAuth, OAuthVault, LoginStart, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let auth =
            OwnerAuth::new(&base, "svc_hub", "ws_test", "https://hub.example/prefix/").unwrap();
        let vault = OAuthVault::default();
        let start = vault.start(&auth, NOW).unwrap();
        let challenge = query(&start)["code_challenge"].clone();
        let role = role.to_owned();
        let join = thread::spawn(move || {
            let responses = [
                json!({"access_token":"synthetic.oauth.signature","token_type":"Bearer","audience":"svc_hub","expires_at":"ignored; signed claims determine expiry"}),
                json!({"active":true,"claims":{"service_id":"svc_hub","workspace_id":"ws_test","sub":"human_owner","principal_kind":"human","exp":NOW+120}}),
                json!({"principals":[{"kind":"human","id":"human_owner","role":role}]}),
            ];
            for (i, response) in responses.into_iter().enumerate() {
                let began = Instant::now();
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(s) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(began.elapsed() < Duration::from_secs(2));
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => panic!("private OAuth listener failed"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = vec![];
                let mut byte = [0; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.extend(byte);
                    assert!(request.len() < 16384);
                }
                let head = String::from_utf8(request).unwrap();
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                assert!(length < 16384);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                if i == 0 {
                    assert!(head.starts_with("POST /api/apps/oauth/token HTTP/1.1"));
                    let form = Url::parse(&format!(
                        "http://form.invalid/?{}",
                        String::from_utf8(body).unwrap()
                    ))
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect::<HashMap<_, _>>();
                    assert_eq!(form["grant_type"], "authorization_code");
                    assert_eq!(form["client_id"], "svc_hub");
                    assert_eq!(
                        form["redirect_uri"],
                        "https://hub.example/prefix/api/auth/callback"
                    );
                    assert_eq!(form["code"], "synthetic-code");
                    assert_eq!(
                        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
                        challenge
                    );
                } else if i == 1 {
                    assert!(head.starts_with("POST /.treer/apps/identity/verify HTTP/1.1"));
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(value["token"], "synthetic.oauth.signature");
                    assert_eq!(value["audience"], "svc_hub");
                } else {
                    assert!(head.starts_with("GET /api/apps/svc_hub/directory HTTP/1.1"));
                    assert!(head
                        .to_ascii_lowercase()
                        .contains("authorization: bearer synthetic.oauth.signature"));
                }
                let bytes = response.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    bytes.len(),
                    bytes
                )
                .unwrap();
            }
        });
        (auth, vault, start, join)
    }
    #[test]
    fn real_exchange_uses_pkce_and_session_cookie_never_contains_the_access_token() {
        let (auth, vault, start, server) = fake_proxy("owner");
        let state = query(&start)["state"].clone();
        let session = vault
            .finish(&auth, "synthetic-code", &state, start.browser_secret(), NOW)
            .unwrap();
        server.join().unwrap();
        assert!(opaque(session.cookie_secret()));
        assert_ne!(session.cookie_secret(), "synthetic.oauth.signature");
        assert_eq!(session.expires_at(), NOW + 120);
        assert_eq!(
            vault
                .session_token(session.cookie_secret(), NOW)
                .unwrap()
                .as_deref(),
            Some("synthetic.oauth.signature")
        );
        assert!(vault
            .finish(&auth, "synthetic-code", &state, start.browser_secret(), NOW)
            .is_err());
        assert!(vault
            .session_token("synthetic.oauth.signature", NOW)
            .unwrap()
            .is_none());
        assert!(vault
            .session_token(session.cookie_secret(), NOW - 1)
            .unwrap()
            .is_none());
        vault.logout(session.cookie_secret()).unwrap();
        assert!(vault
            .session_token(session.cookie_secret(), NOW)
            .unwrap()
            .is_none());
    }
    #[test]
    fn nonowner_exchange_consumes_state_but_never_creates_session() {
        let (auth, vault, start, server) = fake_proxy("member");
        let state = query(&start)["state"].clone();
        let error = vault
            .finish(&auth, "synthetic-code", &state, start.browser_secret(), NOW)
            .err()
            .unwrap();
        server.join().unwrap();
        assert_eq!(error.error, Error::OwnerRequired);
        assert!(error.clear_browser_cookie);
        assert!(vault.state.lock().unwrap().sessions.is_empty());
        assert!(vault.consume(&state, start.browser_secret(), NOW).is_err());
    }
    #[test]
    fn invalid_code_consumes_only_its_matching_browser_flow_and_restart_drops_sessions() {
        let vault = OAuthVault::default();
        let auth = configured();
        let start = vault.start(&auth, NOW).unwrap();
        let state = query(&start)["state"].clone();
        let error = vault
            .finish(&auth, "", &state, start.browser_secret(), NOW)
            .err()
            .unwrap();
        assert!(error.clear_browser_cookie);
        assert!(vault.consume(&state, start.browser_secret(), NOW).is_err());
        let raw = "x".repeat(64);
        vault.state.lock().unwrap().sessions.insert(
            hash(&raw),
            Session {
                token: "synthetic-access-token".into(),
                created_at: NOW,
                expires: NOW + 60,
            },
        );
        assert!(vault.session_token(&raw, NOW).unwrap().is_some());
        assert!(vault.session_token(&raw, NOW + 60).unwrap().is_none());
        assert!(OAuthVault::default()
            .session_token(&raw, NOW)
            .unwrap()
            .is_none());
    }
    #[test]
    fn full_session_vault_refuses_new_login_without_evicting_existing_sessions() {
        let (auth, vault, start, server) = fake_proxy("owner");
        {
            let mut memory = vault.state.lock().unwrap();
            for n in 0..SESSIONS {
                memory.sessions.insert(
                    hash(&format!("synthetic-existing-cookie-{n:040}")),
                    Session {
                        token: "synthetic-existing-token".into(),
                        created_at: NOW,
                        expires: NOW + 60,
                    },
                );
            }
        }
        let state = query(&start)["state"].clone();
        let error = vault
            .finish(&auth, "synthetic-code", &state, start.browser_secret(), NOW)
            .err()
            .unwrap();
        server.join().unwrap();
        assert_eq!(error.error, Error::LoginCapacity);
        assert!(error.clear_browser_cookie);
        assert_eq!(vault.state.lock().unwrap().sessions.len(), 100);
        assert!(vault
            .state
            .lock()
            .unwrap()
            .sessions
            .values()
            .all(|session| session.token == "synthetic-existing-token"));
        assert!(vault.state.lock().unwrap().flows.is_empty());
        assert!(vault
            .session_token(&format!("synthetic-existing-cookie-{:040}", 0), NOW + 60)
            .unwrap()
            .is_none());
        assert!(vault.state.lock().unwrap().sessions.is_empty());
    }
    #[test]
    fn token_exchange_failure_does_not_leave_replayable_state_or_a_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        // Keep the private port bound until its response. Never race another
        // local service by closing a supposedly unused port before connecting.
        let server = thread::spawn(move || {
            let began = Instant::now();
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(s) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(began.elapsed() < Duration::from_secs(2));
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => panic!("private OAuth failure listener failed"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut head = vec![];
            let mut byte = [0; 1];
            while !head.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                head.extend(byte);
                assert!(head.len() < 16384);
            }
            let header = String::from_utf8(head).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            assert!(length < 16384);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            stream
                .write_all(b"HTTP/1.1 503 Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let auth =
            OwnerAuth::new(&base, "svc_hub", "ws_test", "https://hub.example/prefix/").unwrap();
        let vault = OAuthVault::default();
        let start = vault.start(&auth, NOW).unwrap();
        let state = query(&start)["state"].clone();
        let error = vault
            .finish(&auth, "synthetic-code", &state, start.browser_secret(), NOW)
            .err()
            .unwrap();
        assert_eq!(error.error, Error::Proxy);
        server.join().unwrap();
        assert!(error.clear_browser_cookie);
        assert!(vault.state.lock().unwrap().sessions.is_empty());
        assert!(vault.consume(&state, start.browser_secret(), NOW).is_err());
    }
}
