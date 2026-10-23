//! Existing Proxy email route, as this Managed App's own workload principal.
//! No provider secrets, redirects, caller URLs, ambient HTTP proxies or raw
//! reqwest/upstream errors. Construction alone never sends or mints a token.
use super::delivery::{Attempt, Outcome};
use super::{Intent, Mode};
use crate::core::Core;
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde_json::Value;
use std::{io::Read, time::Duration};
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("email_proxy_configuration_invalid")]
    Configuration,
    #[error("email_proxy_unavailable")]
    Unavailable,
    #[error("email_proxy_rejected")]
    Rejected,
    /// Local fence refusal before POST; not a provider retry signal.
    #[error("email_proxy_retry_later")]
    Retry,
    /// A POST was attempted. Do not retry or infer non-send from this result.
    #[error("email_proxy_outcome_uncertain")]
    Uncertain,
}
/// Current visible human principal. Address ownership is deliberately not inferred.
#[derive(serde::Serialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub role: String,
}
fn members(value: &Value) -> Result<Vec<Member>, Error> {
    let principals = value
        .get("principals")
        .and_then(Value::as_array)
        .filter(|p| p.len() <= 1000)
        .ok_or(Error::Unavailable)?;
    let mut ids = std::collections::HashSet::new();
    let mut result = Vec::new();
    for p in principals {
        let id = p
            .get("id")
            .and_then(Value::as_str)
            .ok_or(Error::Unavailable)?;
        if !crate::store::valid_id(id) || id.len() > 128 || !ids.insert(id) {
            return Err(Error::Unavailable);
        }
        if p.get("kind").and_then(Value::as_str) != Some("human") {
            continue;
        }
        let role = p
            .get("role")
            .and_then(Value::as_str)
            .ok_or(Error::Unavailable)?;
        if !matches!(role, "owner" | "member") {
            continue;
        }
        // Legacy directories omit names; stable ID remains an honest label.
        let name = p.get("name").and_then(Value::as_str).unwrap_or(id);
        if name.len() > 512 || name.chars().any(char::is_control) {
            return Err(Error::Unavailable);
        }
        result.push(Member {
            id: id.into(),
            name: name.into(),
            role: role.into(),
        });
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}
pub struct Sender {
    client: Client,
    url: Url,
    service: String,
}
/// A consumed dispatch capability and its conservative outcome. No accessor
/// returns the Attempt, so the normal transport API cannot POST it twice.
/// Dropping this value, or a failed commit, leaves startup recovery to contain
/// the existing dispatching row. This is not permission to acknowledge work.
#[must_use = "persist the completion; dropping it leaves an ambiguous dispatch"]
pub struct Completion {
    attempt: Attempt,
    outcome: Outcome,
}
impl Completion {
    pub fn commit(self, queue: &mut super::Queue<'_>) -> Result<(), super::Error> {
        queue.finish_proxy_delivery(self.attempt, self.outcome)
    }
}
impl Sender {
    /// All inputs come from reviewed startup configuration, never message JSON.
    /// Runtime must check current membership/Core generation and dispatch fence
    /// after token mint and immediately before this HTTP request.
    pub fn new(proxy: &str, service: &str) -> Result<Self, Error> {
        let mut url = Url::parse(proxy).map_err(|_| Error::Configuration)?;
        let loopback = url
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if !service.starts_with("svc_")
            || service.len() > 128
            || !service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
            || url.port() == Some(0)
            || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        {
            return Err(Error::Configuration);
        }
        url.set_path(&format!("/api/apps/{service}/email"));
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| Error::Configuration)?;
        Ok(Self {
            client,
            url,
            service: service.into(),
        })
    }
    /// Current directory visibility is required at owner binding and dispatch.
    /// The caller supplies a stable human ID, never an identity assertion.
    pub fn current_member(&self, core: &Core, member: &str) -> Result<bool, Error> {
        if !crate::store::valid_id(member) || member.len() > 128 {
            return Err(Error::Rejected);
        }
        Ok(self.members(core)?.iter().any(|p| p.id == member))
    }
    /// Read-only, bounded directory lookup using this App's normal service audience.
    pub fn members(&self, core: &Core) -> Result<Vec<Member>, Error> {
        let token = core
            .app_service_token(&self.service)
            .map_err(|_| Error::Unavailable)?;
        let mut url = self.url.clone();
        url.set_path(&format!("/api/apps/{}/directory", self.service));
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(Error::Unavailable);
        }
        let mut bytes = Vec::new();
        response
            .take(262145)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Unavailable)?;
        if bytes.len() > 262144 {
            return Err(Error::Unavailable);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Unavailable)?;
        members(&value)
    }
    /// The fence runs after minting, so a pause or lease expiry while the CLI
    /// was waiting cannot dispatch a newly stale intent. No queue mutex may be
    /// held during the CLI or HTTP work. The callback must not cache positives.
    pub fn send(
        &self,
        core: &Core,
        intent: &Intent,
        public_url: &str,
        fence: impl FnOnce() -> bool,
    ) -> Result<(), Error> {
        intent
            .notification(public_url, Mode::Proxy)
            .map_err(|_| Error::Rejected)?;
        let token = core
            .app_service_token(&self.service)
            .map_err(|_| Error::Unavailable)?;
        if !fence() {
            return Err(Error::Retry);
        }
        self.post(&token, intent, public_url)
    }
    /// Offline assembly primitive: consumes an already committed reservation
    /// and posts its exact stored bytes once. Caller must establish fresh Core,
    /// member, allowlist and activation checks; the post-mint fence must recheck
    /// current pins/lease/pause, without holding a mutex across token mint/HTTP.
    /// Even pre-POST refusal is conservatively uncertain after reservation; it
    /// never returns the Attempt or a retry signal. The opt-in worker supplies these checks.
    pub fn send_committed(
        &self,
        core: &Core,
        attempt: Attempt,
        fence: impl FnOnce() -> bool,
    ) -> Completion {
        let accepted = core
            .app_service_token(&self.service)
            .ok()
            .filter(|_| fence())
            .is_some_and(|token| self.post_bytes(&token, attempt.payload()).is_ok());
        Completion {
            attempt,
            outcome: if accepted {
                Outcome::ProviderAccepted
            } else {
                Outcome::Uncertain
            },
        }
    }
    fn post(&self, token: &str, intent: &Intent, public_url: &str) -> Result<(), Error> {
        let notification = intent
            .notification(public_url, Mode::Proxy)
            .map_err(|_| Error::Rejected)?;
        let payload = serde_json::to_vec(&notification).map_err(|_| Error::Rejected)?;
        self.post_bytes(token, &payload)
    }
    fn post_bytes(&self, token: &str, payload: &[u8]) -> Result<(), Error> {
        let response = self
            .client
            .post(self.url.clone())
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload.to_vec())
            .send()
            .map_err(|_| Error::Uncertain)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(Error::Uncertain);
        }
        let mut bytes = Vec::new();
        response
            .take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Uncertain)?;
        if bytes.len() > 16384 {
            return Err(Error::Uncertain);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Uncertain)?;
        if status.is_success() {
            if value.get("status").and_then(Value::as_str) != Some("sent")
                || value
                    .get("email_id")
                    .and_then(Value::as_str)
                    .is_none_or(|id| {
                        !id.starts_with("email_")
                            || id.len() > 128
                            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    })
            {
                return Err(Error::Uncertain);
            }
            return Ok(());
        }
        // No non-success response proves non-send under the existing route's
        // contract. In particular, in_progress/stale reservations and HTTP 429
        // must not trigger another POST, even after the 60-second Core window.
        Err(Error::Uncertain)
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn email_setup_directory_is_bounded_current_and_not_an_address_claim() {
        use serde_json::json;
        let rows = super::members(&json!({"principals":[
            {"id":"ag_worker","kind":"agent","name":"Agent"},
            {"id":"user_b","kind":"human","role":"member","name":"<b>Member</b>"},
            {"id":"user_a","kind":"human","role":"owner"}
        ]}))
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "user_a");
        assert_eq!(rows[0].name, "user_a");
        assert_eq!(rows[1].name, "<b>Member</b>");
        for value in [
            json!({"principals":[{"id":"user_a","kind":"human","role":"member"},{"id":"user_a","kind":"human","role":"owner"}]}),
            json!({"principals":[{"id":"","kind":"human","role":"member"}]}),
            json!({"principals":[{"id":"user_a","kind":"human","role":"member","name":"bad\nname"}]}),
            json!({"principals":[{"id":"user_a","kind":"human","role":"member","name":"a".repeat(513)}]}),
            json!({"principals":vec![json!({"id":"ag_a","kind":"agent"});1001]}),
            json!({}),
        ] {
            assert!(super::members(&value).is_err());
        }
        assert!(super::members(&json!({"principals":[]}))
            .unwrap()
            .is_empty());
    }

    use super::*;
    use std::{io::Write, net::TcpListener, thread};
    fn intent(address: &str) -> Intent {
        Intent {
            id: "a".repeat(64),
            nonce: "b".repeat(64),
            thread: "c".repeat(64),
            endpoint: "ag_email".into(),
            generation: 1,
            address: address.into(),
        }
    }
    #[test]
    fn proxy_configuration_rejects_untrusted_origin_before_network() {
        for url in [
            "http://example.test/",
            "https://user:secret@example.test/",
            "https://proxy.example/api/",
            "https://proxy.example/?next=evil",
            "https://proxy.example/#secret",
        ] {
            assert!(Sender::new(url, "svc_hub").is_err());
        }
        assert!(Sender::new("https://proxy.example/", "svc_../evil").is_err());
    }
    #[test]
    fn proxy_fixed_route_bearer_summary_and_response_classification() {
        for (status, body, expected) in [
            (200, r#"{"status":"sent","email_id":"email_test"}"#, Ok(())),
            (
                200,
                r#"{"status":"queued","email_id":"email_test"}"#,
                Err(Error::Uncertain),
            ),
            (
                503,
                r#"{"error":{"code":"email_not_configured","message":"SYNTHETIC_SECRET"}}"#,
                Err(Error::Uncertain),
            ),
            (
                503,
                r#"{"error":{"code":"email_in_progress"}}"#,
                Err(Error::Uncertain),
            ),
            (
                403,
                r#"{"error":{"code":"email_recipient_rejected","message":"SYNTHETIC_SECRET"}}"#,
                Err(Error::Uncertain),
            ),
            (302, r#"{}"#, Err(Error::Uncertain)),
            (
                429,
                r#"{"error":{"code":"rate_limited"}}"#,
                Err(Error::Uncertain),
            ),
            (
                500,
                r#"{"error":{"code":"email_delivery_failed"}}"#,
                Err(Error::Uncertain),
            ),
            (
                500,
                r#"{"error":{"code":"email_storage_failed"}}"#,
                Err(Error::Uncertain),
            ),
            (
                200,
                r#"{"status":"sent","email_id":"bad"}"#,
                Err(Error::Uncertain),
            ),
            (200, "not json", Err(Error::Uncertain)),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let sender = Sender::new(
                &format!("http://{}/", listener.local_addr().unwrap()),
                "svc_hub",
            )
            .unwrap();
            let join = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    head.extend(byte);
                    assert!(head.len() < 16384);
                }
                let head = String::from_utf8(head).unwrap();
                assert!(head.starts_with("POST /api/apps/svc_hub/email HTTP/1.1\r\n"));
                assert!(head
                    .to_ascii_lowercase()
                    .contains("authorization: bearer synthetic.app.signature\r\n"));
                let size = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                assert!(size < 4096);
                let mut data = vec![0; size];
                stream.read_exact(&mut data).unwrap();
                let value: Value = serde_json::from_slice(&data).unwrap();
                assert_eq!(value["to"], super::super::REAL_RECIPIENT);
                assert_eq!(value["subject"], "Treer conversation update");
                assert!(!String::from_utf8(data)
                    .unwrap()
                    .contains("synthetic.app.signature"));
                let location = if status == 302 {
                    "Location: http://127.0.0.1:1/never-follow\r\n"
                } else {
                    ""
                };
                write!(stream,"HTTP/1.1 {status} Test\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            assert_eq!(
                sender.post(
                    "synthetic.app.signature",
                    &intent(super::super::REAL_RECIPIENT),
                    "https://hub.example/"
                ),
                expected
            );
            join.join().unwrap();
        }
    }
    #[test]
    fn post_transport_failure_and_unreadable_response_are_uncertain() {
        for reply in [
            String::new(),
            "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}".into(),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: 17000\r\nConnection: close\r\n\r\n{}",
                "x".repeat(17000)
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let sender = Sender::new(
                &format!("http://{}/", listener.local_addr().unwrap()),
                "svc_hub",
            )
            .unwrap();
            let join = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    head.extend(byte);
                    assert!(head.len() < 16384);
                }
                let head = String::from_utf8(head).unwrap();
                let size = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                let mut data = vec![0; size];
                stream.read_exact(&mut data).unwrap();
                // Model connection loss, a truncated success, and an oversized
                // response after the server has consumed the complete POST.
                let _ = stream.write_all(reply.as_bytes());
            });
            assert_eq!(
                sender.post(
                    "synthetic.app.signature",
                    &intent(super::super::REAL_RECIPIENT),
                    "https://hub.example/"
                ),
                Err(Error::Uncertain)
            );
            join.join().unwrap();
        }
    }
    #[test]
    fn proxy_dispatch_rechecks_fence_after_own_token_mint() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("fake-treer");
        std::fs::write(&exe,r#"#!/usr/bin/python3
import json,sys
assert sys.argv[1:]==['token','create','svc_hub','--json']
print(json.dumps({'access_token':'synthetic.app.signature','token_type':'Bearer','audience':'svc_hub','expires_in':300}))
"#).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let core = Core::new(&exe, "app_own").unwrap();
        let sender = Sender::new("http://127.0.0.1:1/", "svc_hub").unwrap();
        assert_eq!(
            sender.send(
                &core,
                &intent(super::super::REAL_RECIPIENT),
                "https://hub.example/",
                || false
            ),
            Err(Error::Retry)
        );
    }
}
