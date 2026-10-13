use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde::de::DeserializeOwned;
use serde::Serialize;
use treer_protocol::{
    AgentInterfaceDescriptor, AgentInterfaceManifest, AgentInterfaceStatusResponse,
    AgentTranscriptResponse, ProtocolError,
};

use crate::network::connect_agent_service;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

pub async fn manifest(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
) -> Result<AgentInterfaceManifest, ProtocolError> {
    request::<serde_json::Value, AgentInterfaceManifest>(
        agent_id,
        interface,
        Method::GET,
        "/v1/manifest",
        None,
    )
    .await
}

pub async fn abort(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    operation_id: &str,
) -> Result<(), ProtocolError> {
    let _: serde_json::Value = request(
        agent_id,
        interface,
        Method::POST,
        "/v1/abort",
        Some(&serde_json::json!({ "operation_id": operation_id })),
    )
    .await?;
    Ok(())
}

pub async fn submit_prompt(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    operation_id: &str,
    text: &str,
) -> Result<(), ProtocolError> {
    let _: serde_json::Value = request(
        agent_id,
        interface,
        Method::POST,
        "/v1/prompts",
        Some(&serde_json::json!({
            "operation_id": operation_id,
            "text": text,
            "mode": "prompt",
            "delivery": "steer"
        })),
    )
    .await?;
    Ok(())
}

pub async fn submit_delivery(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    body: &treer_protocol::AgentInterfacePromptRequest,
) -> Result<treer_protocol::AgentInterfacePromptResponse, ProtocolError> {
    request(agent_id, interface, Method::POST, "/v1/prompts", Some(body)).await
}

pub async fn queue_request(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    submission_id: Option<&str>,
    cancel: bool,
) -> Result<serde_json::Value, ProtocolError> {
    let path = match submission_id {
        Some(id) => format!(
            "/v1/{}/{}",
            if cancel { "queue" } else { "submissions" },
            percent_encoding::utf8_percent_encode(id, percent_encoding::NON_ALPHANUMERIC)
        ),
        None => "/v1/queue".to_string(),
    };
    request::<serde_json::Value, serde_json::Value>(
        agent_id,
        interface,
        if cancel { Method::DELETE } else { Method::GET },
        &path,
        None,
    )
    .await
}

pub async fn transcript(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> Result<AgentTranscriptResponse, ProtocolError> {
    let mut path = "/v1/transcript".to_string();
    let mut separator = '?';
    if let Some(cursor) = cursor {
        let encoded =
            percent_encoding::utf8_percent_encode(cursor, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        path.push(separator);
        separator = '&';
        path.push_str("page=");
        path.push_str(&encoded);
        path.push('&');
        path.push_str("cursor=");
        path.push_str(&encoded);
    }
    if let Some(limit) = limit {
        path.push(separator);
        path.push_str(&format!("limit={}", limit.min(1000)));
    }
    request::<serde_json::Value, AgentTranscriptResponse>(
        agent_id,
        interface,
        Method::GET,
        &path,
        None,
    )
    .await
}

pub async fn status(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
) -> Result<AgentInterfaceStatusResponse, ProtocolError> {
    let status = request::<serde_json::Value, AgentInterfaceStatusResponse>(
        agent_id,
        interface,
        Method::GET,
        "/v1/status",
        None,
    )
    .await?;
    if status.agent_id != agent_id || status.interface_instance_id != interface.instance_id {
        return Err(ProtocolError::new(
            "agent_interface_identity_mismatch",
            "Agent Interface status identity does not match its registration",
        ));
    }
    Ok(status)
}

async fn request<B: Serialize + ?Sized, T: DeserializeOwned>(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    method: Method,
    path: &str,
    body: Option<&B>,
) -> Result<T, ProtocolError> {
    let future = async {
        let stream = connect_agent_service(agent_id, interface.port)
            .await
            .map_err(unavailable)?;
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(unavailable)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let bytes = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|error| {
                ProtocolError::new("agent_interface_request_invalid", error.to_string())
            })?
            .unwrap_or_default();
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "agent-interface")
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("x-treer-agent-id", agent_id)
            .header(
                treer_protocol::AGENT_INTERFACE_INSTANCE_HEADER,
                &interface.instance_id,
            )
            .header(
                treer_protocol::AGENT_INTERFACE_SOURCE_HEADER,
                treer_protocol::AGENT_INTERFACE_SOURCE_CONTROLLER,
            )
            .body(Full::new(Bytes::from(bytes)))
            .map_err(unavailable)?;
        let response = sender.send_request(request).await.map_err(unavailable)?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(unavailable)?
            .to_bytes();
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(ProtocolError::new(
                "agent_interface_response_too_large",
                "Agent Interface response exceeded 8 MiB",
            ));
        }
        if !status.is_success() {
            let detail = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("error").cloned())
                .map_or_else(
                    || String::from_utf8_lossy(&body).into_owned(),
                    |value| value.to_string(),
                );
            return Err(ProtocolError::new(
                "agent_interface_rejected",
                format!("Agent Interface returned {status}: {detail}"),
            ));
        }
        serde_json::from_slice(&body).map_err(|error| {
            ProtocolError::new("agent_interface_response_invalid", error.to_string())
        })
    };
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .map_err(|_| {
            ProtocolError::new("agent_interface_unavailable", "Agent Interface timed out")
        })?
}

fn unavailable(error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new("agent_interface_unavailable", error.to_string())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use treer_protocol::AGENT_INTERFACE_PROTOCOL_V1;

    use super::*;

    #[tokio::test]
    async fn queue_delivery_carries_attestation_and_returns_specific_receipt() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for (method, path, response) in [
                (
                    "POST",
                    "/v1/prompts",
                    serde_json::json!({"submission":{"id":"sub-1","disposition":"queued","position":1}}),
                ),
                (
                    "GET",
                    "/v1/submissions/sub%2D1",
                    serde_json::json!({"submission":{"id":"sub-1","state":"running"}}),
                ),
                (
                    "DELETE",
                    "/v1/queue/sub%2D1",
                    serde_json::json!({"submission":{"id":"sub-1","state":"interrupted"}}),
                ),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    let Some(offset) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&bytes[..offset]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() < offset + 4 + length {
                        continue;
                    }
                    assert!(headers.starts_with(&format!("{method} {path} ").to_lowercase()));
                    assert!(headers.contains("x-treer-agent-id: target"));
                    assert!(headers.contains("x-treer-interface-instance: instance"));
                    assert!(headers.contains("x-treer-interface-source: controller-api"));
                    assert!(headers.contains("host: agent-interface"));
                    assert!(!headers.lines().any(|line| line.starts_with("origin:")));
                    if method == "POST" {
                        let body: serde_json::Value =
                            serde_json::from_slice(&bytes[offset + 4..]).unwrap();
                        assert_eq!(body["delivery"], "queue");
                        assert_eq!(body["sender"]["agent_id"], "sender");
                        assert_eq!(body["notify"]["to"], "sender");
                        assert_eq!(body["operation_id"], "operation-1");
                    }
                    break;
                }
                let body = response.to_string();
                socket.write_all(format!("HTTP/1.1 202 Accepted\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
            }
        });
        let interface = AgentInterfaceDescriptor {
            protocol: AGENT_INTERFACE_PROTOCOL_V1.into(),
            instance_id: "instance".into(),
            port,
            capabilities: vec!["prompt.queue".into()],
            ui_path: None,
            registered_at: Utc::now(),
        };
        let request = treer_protocol::AgentInterfacePromptRequest {
            operation_id: "operation-1".into(),
            text: "task".into(),
            mode: "prompt".into(),
            delivery: treer_protocol::PromptDelivery::Queue,
            sender: Some(treer_protocol::PromptSender {
                agent_id: "sender".into(),
                path: "/team/sender".into(),
                name: "sender".into(),
            }),
            notify: Some(treer_protocol::PromptNotify {
                to: Some("sender".into()),
                ..Default::default()
            }),
        };
        let result = submit_delivery("target", &interface, &request)
            .await
            .unwrap();
        assert_eq!(result.submission.id, "sub-1");
        assert_eq!(result.submission.position, Some(1));
        assert_eq!(
            queue_request("target", &interface, Some("sub-1"), false)
                .await
                .unwrap()["submission"]["state"],
            "running"
        );
        assert_eq!(
            queue_request("target", &interface, Some("sub-1"), true)
                .await
                .unwrap()["submission"]["state"],
            "interrupted"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reads_and_validates_a_loopback_manifest() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test AIS");
        let port = listener.local_addr().expect("AIS address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept manifest request");
            let mut request = vec![0_u8; 2048];
            let count = socket.read(&mut request).await.expect("read request");
            assert!(String::from_utf8_lossy(&request[..count]).starts_with("GET /v1/manifest "));
            let body = serde_json::json!({
                "protocol": AGENT_INTERFACE_PROTOCOL_V1,
                "instance_id": "pi-test",
                "capabilities": ["prompt.submit"],
                "ui_path": "/"
            })
            .to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .await
                .expect("write response");
        });
        let descriptor = AgentInterfaceDescriptor {
            protocol: AGENT_INTERFACE_PROTOCOL_V1.to_string(),
            instance_id: "pi-test".to_string(),
            port,
            capabilities: vec!["prompt.submit".to_string()],
            ui_path: Some("/".to_string()),
            registered_at: Utc::now(),
        };
        let value = manifest("agent-test", &descriptor)
            .await
            .expect("read manifest");
        assert_eq!(value.instance_id, "pi-test");
        server.await.expect("test server");
    }

    #[tokio::test]
    async fn requests_transcript_pages_by_turn() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test AIS");
        let port = listener.local_addr().expect("AIS address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept transcript request");
            let mut request = vec![0_u8; 2048];
            let count = socket.read(&mut request).await.expect("read request");
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET /v1/transcript?page=2&cursor=2&limit=1 "));
            let body = serde_json::json!({
                "agent_id": "agent-test",
                "interface_instance_id": "pi-test",
                "page": 2,
                "page_count": 4,
                "next_page": 3,
                "cursor": "2",
                "next_cursor": "3",
                "entries": []
            })
            .to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                )
                .await
                .expect("write response");
        });
        let descriptor = AgentInterfaceDescriptor {
            protocol: AGENT_INTERFACE_PROTOCOL_V1.to_string(),
            instance_id: "pi-test".to_string(),
            port,
            capabilities: vec!["transcript.read".to_string()],
            ui_path: None,
            registered_at: Utc::now(),
        };
        let value = transcript("agent-test", &descriptor, Some("2"), Some(1))
            .await
            .expect("read transcript");
        assert_eq!(value.page, Some(2));
        assert_eq!(value.page_count, Some(4));
        assert_eq!(value.next_page, Some(3));
        server.await.expect("test server");
    }
}
