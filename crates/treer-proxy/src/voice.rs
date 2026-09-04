use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use uuid::Uuid;

const DEFAULT_MODEL: &str = "qwen3-asr-flash-realtime";
const DEFAULT_URL: &str = "wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime";
const SAMPLE_RATE: u32 = 16_000;
const ENCODING: &str = "pcm16";
const MAX_SESSION: Duration = Duration::from_secs(90);
const MAX_FRAME_BYTES: usize = 64 * 1024;

type QwenStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub struct VoiceAsrConfig {
    provider: Option<String>,
    api_key: Option<String>,
    url: String,
    model: String,
}

impl std::fmt::Debug for VoiceAsrConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceAsrConfig")
            .field("provider", &self.provider)
            .field(
                "api_key_set",
                &self.api_key.as_ref().is_some_and(|key| !key.is_empty()),
            )
            .field("url", &self.url)
            .field("model", &self.model)
            .finish()
    }
}

impl VoiceAsrConfig {
    pub fn from_env() -> Self {
        let provider = env_nonempty("TREER_VOICE_ASR_PROVIDER");
        let api_key =
            env_nonempty("TREER_VOICE_ASR_API_KEY").or_else(|| env_nonempty("DASHSCOPE_API_KEY"));
        Self {
            provider,
            api_key,
            url: env_nonempty("TREER_VOICE_ASR_URL").unwrap_or_else(|| DEFAULT_URL.to_string()),
            model: env_nonempty("TREER_VOICE_ASR_MODEL")
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            provider: None,
            api_key: None,
            url: DEFAULT_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
        }
    }

    #[cfg(test)]
    pub fn qwen_for_test(api_key: &str, url: &str) -> Self {
        Self {
            provider: Some("qwen".to_string()),
            api_key: Some(api_key.to_string()),
            url: url.to_string(),
            model: DEFAULT_MODEL.to_string(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.provider.as_deref() == Some("qwen")
            && self.api_key.as_ref().is_some_and(|key| !key.is_empty())
    }

    pub fn status_json(&self) -> Value {
        json!({
            "enabled": self.enabled(),
            "provider": if self.enabled() { self.provider.clone() } else { None },
            "sample_rate": SAMPLE_RATE,
            "encoding": ENCODING,
        })
    }

    fn upstream_url(&self) -> String {
        with_model_query(&self.url, &self.model)
    }
}

#[derive(Clone, Debug)]
pub struct VoiceServices {
    pub asr: VoiceAsrConfig,
    pub llm: crate::voice_llm::VoiceLlmConfig,
}

impl VoiceServices {
    pub fn from_env() -> Self {
        Self {
            asr: VoiceAsrConfig::from_env(),
            llm: crate::voice_llm::VoiceLlmConfig::from_env(),
        }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            asr: VoiceAsrConfig::disabled(),
            llm: crate::voice_llm::VoiceLlmConfig::disabled(),
        }
    }

    #[cfg(test)]
    pub fn with_llm(llm: crate::voice_llm::VoiceLlmConfig) -> Self {
        Self {
            asr: VoiceAsrConfig::disabled(),
            llm,
        }
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn with_model_query(url: &str, model: &str) -> String {
    if url.contains("model=") {
        return url.to_string();
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}model={model}")
}

pub async fn proxy_qwen_asr(client: WebSocket, config: VoiceAsrConfig) {
    if let Err(error) = run_qwen_asr(client, config).await {
        tracing::warn!(%error, "voice asr session ended");
    }
}

async fn run_qwen_asr(client: WebSocket, config: VoiceAsrConfig) -> anyhow::Result<()> {
    let (mut client_out, mut client_in) = client.split();
    let deadline = tokio::time::sleep(MAX_SESSION);
    tokio::pin!(deadline);
    let mut finishing = false;
    let mut announced = false;

    'hold: loop {
        let (mut upstream_out, mut upstream_in) = match connect_qwen(&config).await {
            Ok(pair) => pair,
            Err(error) => {
                let _ = send_client(&mut client_out, client_error(&error.to_string())).await;
                return Err(error);
            }
        };
        if let Err(error) = send_json(&mut upstream_out, session_update()).await {
            if finishing {
                break;
            }
            tracing::warn!(%error, "qwen asr session.update failed; retrying");
            continue;
        }
        if !announced {
            send_client(
                &mut client_out,
                json!({ "type": "ready", "sample_rate": SAMPLE_RATE, "encoding": ENCODING }),
            )
            .await?;
            announced = true;
        }

        let reconnect = loop {
            tokio::select! {
                _ = &mut deadline, if !finishing => {
                    finishing = true;
                    let _ = send_json(&mut upstream_out, finish_event()).await;
                }
                message = client_in.next() => {
                    match message {
                        Some(Ok(Message::Binary(data))) => {
                            if data.len() > MAX_FRAME_BYTES {
                                let _ = send_client(&mut client_out, client_error("audio frame too large")).await;
                                finishing = true;
                                break false;
                            }
                            if !data.is_empty()
                                && send_json(&mut upstream_out, append_event(&data))
                                    .await
                                    .is_err()
                            {
                                break !finishing;
                            }
                        }
                        Some(Ok(Message::Text(text))) => {
                            if client_wants_stop(&text) {
                                finishing = true;
                                let _ = send_json(&mut upstream_out, finish_event()).await;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            finishing = true;
                            break false;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) => {
                            finishing = true;
                            break false;
                        }
                    }
                }
                message = upstream_in.next() => {
                    match message {
                        Some(Ok(UpstreamMessage::Text(text))) => {
                            if let Some(event) = map_qwen_event(&text) {
                                let ended = event["type"] == "closed";
                                if event["type"] != "closed" {
                                    let _ = send_client(&mut client_out, event).await;
                                }
                                if ended {
                                    break !finishing;
                                }
                            }
                        }
                        Some(Ok(UpstreamMessage::Close(_))) | None => break !finishing,
                        Some(Ok(_)) => {}
                        Some(Err(_)) => break !finishing,
                    }
                }
            }
        };
        let _ = upstream_out.send(UpstreamMessage::Close(None)).await;
        if !reconnect {
            break 'hold;
        }
        tracing::info!("qwen asr upstream ended during hold; reconnecting");
    }
    let _ = send_client(&mut client_out, json!({ "type": "closed" })).await;
    Ok(())
}

async fn connect_qwen(
    config: &VoiceAsrConfig,
) -> anyhow::Result<(
    futures_util::stream::SplitSink<QwenStream, UpstreamMessage>,
    futures_util::stream::SplitStream<QwenStream>,
)> {
    let api_key = config
        .api_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("qwen asr key missing"))?;
    let mut request = config
        .upstream_url()
        .into_client_request()
        .map_err(|error| anyhow::anyhow!("invalid qwen asr url: {error}"))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|error| anyhow::anyhow!("invalid qwen asr key: {error}"))?,
    );
    let (upstream, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|error| anyhow::anyhow!("qwen asr connect failed: {error}"))?;
    Ok(upstream.split())
}

fn session_update() -> Value {
    json!({
        "event_id": Uuid::new_v4().to_string(),
        "type": "session.update",
        "session": {
            "input_audio_format": "pcm",
            "sample_rate": SAMPLE_RATE,
            "turn_detection": {
                "type": "server_vad",
                "threshold": 0.0,
                "silence_duration_ms": 2000
            }
        }
    })
}

async fn send_json<S>(sink: &mut S, value: Value) -> anyhow::Result<()>
where
    S: SinkExt<UpstreamMessage> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let encoded = serde_json::to_string(&value)?;
    sink.send(UpstreamMessage::Text(encoded.into()))
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(())
}

async fn send_client<S>(sink: &mut S, value: Value) -> anyhow::Result<()>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let encoded = serde_json::to_string(&value)?;
    sink.send(Message::Text(encoded.into()))
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(())
}

fn append_event(pcm: &[u8]) -> Value {
    json!({
        "event_id": Uuid::new_v4().to_string(),
        "type": "input_audio_buffer.append",
        "audio": BASE64.encode(pcm),
    })
}

fn finish_event() -> Value {
    json!({
        "event_id": Uuid::new_v4().to_string(),
        "type": "session.finish",
    })
}

fn client_wants_stop(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|kind| kind == "stop" || kind == "finish")
}

fn client_error(message: &str) -> Value {
    json!({ "type": "error", "message": message })
}

pub(crate) fn map_qwen_event(raw: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let kind = value.get("type")?.as_str()?;
    match kind {
        "conversation.item.input_audio_transcription.text" => {
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let stash = value
                .get("stash")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let combined = format!("{text}{stash}");
            if combined.is_empty() {
                None
            } else {
                Some(json!({ "type": "partial", "text": combined }))
            }
        }
        "conversation.item.input_audio_transcription.completed" => {
            let text = value
                .get("transcript")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if text.is_empty() {
                None
            } else {
                Some(json!({ "type": "final", "text": text }))
            }
        }
        "error" => {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .unwrap_or("qwen asr error");
            Some(client_error(message))
        }
        "session.finished" => Some(json!({ "type": "closed" })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_without_provider_or_key() {
        let config = VoiceAsrConfig::disabled();
        assert!(!config.enabled());
        assert_eq!(config.status_json()["enabled"], false);
        assert!(config.status_json()["provider"].is_null());
    }

    #[test]
    fn qwen_enabled_when_key_present() {
        let config = VoiceAsrConfig::qwen_for_test("sk-test", DEFAULT_URL);
        assert!(config.enabled());
        assert_eq!(config.status_json()["provider"], "qwen");
        assert_eq!(config.status_json()["sample_rate"], SAMPLE_RATE);
        assert_eq!(config.status_json()["encoding"], ENCODING);
    }

    #[test]
    fn appends_model_query_once() {
        assert_eq!(
            with_model_query("wss://example/ws", "qwen3-asr-flash-realtime"),
            "wss://example/ws?model=qwen3-asr-flash-realtime"
        );
        assert_eq!(
            with_model_query("wss://example/ws?model=qwen3-asr-flash-realtime", "ignored"),
            "wss://example/ws?model=qwen3-asr-flash-realtime"
        );
    }

    #[test]
    fn maps_partial_and_final_without_hotwords() {
        let partial = map_qwen_event(
            r#"{"type":"conversation.item.input_audio_transcription.text","text":"ask claude about llm ","stash":"context"}"#,
        )
        .expect("partial");
        assert_eq!(partial["type"], "partial");
        assert_eq!(partial["text"], "ask claude about llm context");

        let final_event = map_qwen_event(
            r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"prompt reviewer on build-machine"}"#,
        )
        .expect("final");
        assert_eq!(final_event["type"], "final");
        assert_eq!(final_event["text"], "prompt reviewer on build-machine");
    }

    #[test]
    fn stop_json_is_recognized() {
        assert!(client_wants_stop(r#"{"type":"stop"}"#));
        assert!(!client_wants_stop("hello"));
    }
}
