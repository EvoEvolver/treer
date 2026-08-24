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
    transparent_networking: bool,
) -> Result<AgentInterfaceManifest, ProtocolError> {
    request::<serde_json::Value, AgentInterfaceManifest>(
        agent_id,
        interface,
        Method::GET,
        "/v1/manifest",
        None,
        transparent_networking,
    )
    .await
}

pub async fn submit_prompt(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    operation_id: &str,
    text: &str,
    transparent_networking: bool,
) -> Result<(), ProtocolError> {
    let _: serde_json::Value = request(
        agent_id,
        interface,
        Method::POST,
        "/v1/prompts",
        Some(&serde_json::json!({
            "operation_id": operation_id,
            "text": text,
            "mode": "prompt"
        })),
        transparent_networking,
    )
    .await?;
    Ok(())
}

pub async fn transcript(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    cursor: Option<&str>,
    limit: Option<usize>,
    transparent_networking: bool,
) -> Result<AgentTranscriptResponse, ProtocolError> {
    let mut path = "/v1/transcript".to_string();
    let mut separator = '?';
    if let Some(cursor) = cursor {
        path.push(separator);
        separator = '&';
        path.push_str("cursor=");
        path.push_str(
            &percent_encoding::utf8_percent_encode(cursor, percent_encoding::NON_ALPHANUMERIC)
                .to_string(),
        );
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
        transparent_networking,
    )
    .await
}

pub async fn status(
    agent_id: &str,
    interface: &AgentInterfaceDescriptor,
    transparent_networking: bool,
) -> Result<AgentInterfaceStatusResponse, ProtocolError> {
    let status = request::<serde_json::Value, AgentInterfaceStatusResponse>(
        agent_id,
        interface,
        Method::GET,
        "/v1/status",
        None,
        transparent_networking,
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
    transparent_networking: bool,
) -> Result<T, ProtocolError> {
    let future = async {
        let stream = connect_agent_service(agent_id, interface.port, transparent_networking)
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
            .header("x-treer-interface-instance", &interface.instance_id)
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
        let value = manifest("agent-test", &descriptor, false)
            .await
            .expect("read manifest");
        assert_eq!(value.instance_id, "pi-test");
        server.await.expect("test server");
    }
}
