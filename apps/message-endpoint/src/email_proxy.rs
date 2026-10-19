//! Existing Proxy email route, as this Managed App's own workload principal.
//! No provider secrets, redirects, caller URLs, ambient HTTP proxies or raw
//! reqwest/upstream errors. Construction alone never sends or mints a token.
use super::{Intent, Mode};
use crate::core::Core;
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde_json::Value;
use std::{io::Read, time::Duration};
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("email_proxy_configuration_invalid")]
    Configuration,
    #[error("email_proxy_unavailable")]
    Unavailable,
    #[error("email_proxy_rejected")]
    Rejected,
    #[error("email_proxy_retry_later")]
    Retry,
}
pub struct Sender {
    client: Client,
    url: Url,
    service: String,
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
            .timeout(Duration::from_secs(20))
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
        let principals = value
            .get("principals")
            .and_then(Value::as_array)
            .filter(|p| p.len() <= 1000)
            .ok_or(Error::Unavailable)?;
        let matches = principals
            .iter()
            .filter(|p| p.get("id").and_then(Value::as_str) == Some(member))
            .collect::<Vec<_>>();
        Ok(matches.len() == 1
            && matches[0].get("kind").and_then(Value::as_str) == Some("human")
            && matches!(
                matches[0].get("role").and_then(Value::as_str),
                Some("owner" | "member")
            ))
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
    fn post(&self, token: &str, intent: &Intent, public_url: &str) -> Result<(), Error> {
        let notification = intent
            .notification(public_url, Mode::Proxy)
            .map_err(|_| Error::Rejected)?;
        let response = self
            .client
            .post(self.url.clone())
            .bearer_auth(token)
            .json(&notification)
            .send()
            .map_err(|_| Error::Retry)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(Error::Rejected);
        }
        let mut bytes = Vec::new();
        response
            .take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Retry)?;
        if bytes.len() > 16384 {
            return Err(Error::Retry);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Retry)?;
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
                return Err(Error::Retry);
            }
            return Ok(());
        }
        match value.pointer("/error/code").and_then(Value::as_str) {
            Some("email_not_configured") => Err(Error::Unavailable),
            Some("email_in_progress") => Err(Error::Retry),
            _ if status.is_redirection() => Err(Error::Rejected),
            _ if status.is_server_error() || status.as_u16() == 429 => Err(Error::Retry),
            _ => Err(Error::Rejected),
        }
    }
}
#[cfg(test)]
mod tests {
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
    fn proxy_configuration_and_real_recipient_cap_fail_before_network() {
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
        let sender = Sender::new("http://127.0.0.1:1/", "svc_hub").unwrap();
        assert_eq!(
            sender.post(
                "synthetic.app.signature",
                &intent("not-allowed@example.test"),
                "https://hub.example/"
            ),
            Err(Error::Rejected)
        );
    }
    #[test]
    fn proxy_fixed_route_bearer_summary_and_response_classification() {
        for (status, body, expected) in [
            (200, r#"{"status":"sent","email_id":"email_test"}"#, Ok(())),
            (
                200,
                r#"{"status":"queued","email_id":"email_test"}"#,
                Err(Error::Retry),
            ),
            (
                503,
                r#"{"error":{"code":"email_not_configured","message":"SYNTHETIC_SECRET"}}"#,
                Err(Error::Unavailable),
            ),
            (
                503,
                r#"{"error":{"code":"email_in_progress"}}"#,
                Err(Error::Retry),
            ),
            (
                403,
                r#"{"error":{"code":"email_recipient_rejected","message":"SYNTHETIC_SECRET"}}"#,
                Err(Error::Rejected),
            ),
            (302, r#"{}"#, Err(Error::Rejected)),
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
