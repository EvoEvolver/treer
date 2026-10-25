//! Closed loopback provider contracts: no real mail, credentials or database.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn provider(
    response: Vec<u8>,
    hold: bool,
) -> (CloudflareEmailSender, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let n = socket.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..n]);
            if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length: usize = header
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let _ = socket.write_all(&response).await;
        if hold {
            tokio::time::sleep(StdDuration::from_secs(1)).await;
        }
        request
    });
    (
        CloudflareEmailSender {
            client: CloudflareEmailSender::client_builder()
                .no_proxy()
                .timeout(StdDuration::from_millis(300))
                .build()
                .unwrap(),
            endpoint: Url::parse(&format!("http://{address}/send")).unwrap(),
            api_token: "synthetic-token".into(),
            from: "sender@example.test".into(),
        },
        server,
    )
}

async fn send(sender: &CloudflareEmailSender) -> Result<(), EmailSendFailure> {
    sender
        .send(
            "recipient@example.test",
            "subject",
            "<p>synthetic</p>".into(),
            "synthetic".into(),
        )
        .await
}

#[tokio::test]
async fn email_sender_safe_categories_and_known_status() {
    for (status, body, declared_length, hold, category) in [
        (503, "SECRET".to_owned(), 999, true, "http_status"),
        (200, "SECRET".into(), 6, false, "response_invalid"),
        (200, "{}".into(), 2, false, "response_invalid"),
        (200, "SECRET".into(), 99, false, "response_read"),
        (200, "SECRET".into(), 99, true, "timeout"),
        (
            200,
            r#"{"success":false,"errors":[{"message":"SECRET"}]}"#.into(),
            55,
            false,
            "provider_rejected",
        ),
        (200, "x".repeat(65537), 65537, false, "response_too_large"),
    ] {
        // The refusal case uses its exact length; other declared lengths test truncation.
        let length = if category == "provider_rejected" {
            body.len()
        } else {
            declared_length
        };
        let response =
            format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {length}\r\n\r\n{body}");
        let (sender, server) = provider(response.into_bytes(), hold).await;
        let failure = send(&sender).await.unwrap_err();
        assert_eq!(failure.category, category);
        assert_eq!(failure.upstream_status, Some(status));
        for output in [format!("{failure}"), format!("{failure:?}")] {
            for secret in [
                "SECRET",
                "synthetic-token",
                "recipient@",
                "/send",
                "http://",
            ] {
                assert!(!output.contains(secret));
            }
        }
        server.abort();
    }
}

#[tokio::test]
async fn email_sender_timeout_before_headers_has_no_status() {
    let (sender, server) = provider(Vec::new(), true).await;
    let failure = send(&sender).await.unwrap_err();
    assert_eq!(failure.category, "timeout");
    assert_eq!(failure.upstream_status, None);
    server.abort();
}

#[tokio::test]
async fn email_sender_transport_failure_has_no_status() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        drop(listener.accept().await.unwrap());
    });
    let sender = CloudflareEmailSender {
        client: CloudflareEmailSender::client_builder()
            .no_proxy()
            .build()
            .unwrap(),
        endpoint: Url::parse(&format!("http://{address}/send")).unwrap(),
        api_token: "synthetic".into(),
        from: "sender@example.test".into(),
    };
    let failure = send(&sender).await.unwrap_err();
    assert_eq!(failure.category, "transport");
    assert_eq!(failure.upstream_status, None);
    server.await.unwrap();
}

#[tokio::test]
async fn email_sender_redirect_never_replays_post() {
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let response = format!(
        "HTTP/1.1 307 Redirect\r\nLocation: http://{}/leak\r\nContent-Length: 0\r\n\r\n",
        sink.local_addr().unwrap()
    );
    let (sender, server) = provider(response.into_bytes(), false).await;
    let failure = send(&sender).await.unwrap_err();
    assert_eq!(failure.category, "http_status");
    assert_eq!(failure.upstream_status, Some(307));
    server.await.unwrap();
    assert!(
        tokio::time::timeout(StdDuration::from_millis(50), sink.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn email_sender_success_and_fixed_request_contract() {
    let mut sender = CloudflareEmailSender::new(CloudflareEmailConfig {
        account_id: "synthetic-account".into(),
        api_token: "synthetic-token".into(),
        from: "sender@example.test".into(),
    })
    .unwrap();
    assert_eq!(
        sender.endpoint.as_str(),
        "https://api.cloudflare.com/client/v4/accounts/synthetic-account/email/sending/send"
    );
    let body = r#"{"success":true,"errors":[],"result":{"delivered":["recipient@example.test"]}}"#;
    let (fixture, server) = provider(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
        false,
    )
    .await;
    sender.client = fixture.client;
    // Change only the destination to the owned fixture; exercise the exact
    // production path on the wire rather than a fixture-only /send route.
    sender.endpoint.set_scheme("http").unwrap();
    sender
        .endpoint
        .set_host(fixture.endpoint.host_str())
        .unwrap();
    sender.endpoint.set_port(fixture.endpoint.port()).unwrap();
    send(&sender).await.unwrap();
    let request = server.await.unwrap();
    let boundary = request.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
    let headers = String::from_utf8_lossy(&request[..boundary]);
    assert!(headers
        .starts_with("POST /client/v4/accounts/synthetic-account/email/sending/send HTTP/1.1"));
    assert!(headers
        .to_ascii_lowercase()
        .contains("authorization: bearer synthetic-token"));
    let actual: Value = serde_json::from_slice(&request[boundary + 4..]).unwrap();
    assert_eq!(
        actual,
        json!({"to":"recipient@example.test","from":"sender@example.test","subject":"subject","html":"<p>synthetic</p>","text":"synthetic"})
    );
}
