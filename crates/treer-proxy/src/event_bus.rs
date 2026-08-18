use std::fmt;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};
use treer_protocol::DomainEventEnvelope;

const LOCAL_SUBSCRIBER_CAPACITY: usize = 512;
const DEFAULT_PUBLISH_QUEUE_CAPACITY: usize = 4_096;
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const EVENT_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const DEDUPLICATION_WINDOW: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
pub struct EventBusConfig {
    pub nats_url: String,
    pub stream_name: String,
    pub subject_prefix: String,
    pub publish_queue_capacity: usize,
}

impl EventBusConfig {
    pub fn new(nats_url: String, stream_name: String, subject_prefix: String) -> Self {
        Self {
            nats_url,
            stream_name,
            subject_prefix,
            publish_queue_capacity: DEFAULT_PUBLISH_QUEUE_CAPACITY,
        }
    }

    fn validate(&self) -> Result<()> {
        validate_subject_prefix(&self.subject_prefix)?;
        if self.stream_name.is_empty()
            || self
                .stream_name
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || b".*>/\\".contains(&byte))
        {
            bail!("NATS stream name must not contain whitespace, '.', '*', '>', '/', or '\\'");
        }
        if self.publish_queue_capacity == 0 {
            bail!("NATS publish queue capacity must be greater than zero");
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct EventBus {
    local: broadcast::Sender<DomainEventEnvelope>,
    nats: Option<mpsc::Sender<DomainEventEnvelope>>,
}

impl EventBus {
    pub fn in_process() -> Self {
        let (local, _) = broadcast::channel(LOCAL_SUBSCRIBER_CAPACITY);
        Self { local, nats: None }
    }

    pub async fn connect_nats(config: EventBusConfig) -> Result<Self> {
        config.validate()?;
        let client = async_nats::connect(&config.nats_url)
            .await
            .context("failed to connect to configured NATS server")?;
        let jetstream = jetstream::new(client);
        let stream_subject = format!("{}.>", config.subject_prefix);
        let mut stream = jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: config.stream_name.clone(),
                description: Some("Treer durable domain events".to_string()),
                subjects: vec![stream_subject.clone()],
                max_age: EVENT_MAX_AGE,
                max_bytes: 1024 * 1024 * 1024,
                duplicate_window: DEDUPLICATION_WINDOW,
                storage: jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await
            .with_context(|| {
                format!(
                    "failed to create or inspect NATS stream {}",
                    config.stream_name
                )
            })?;
        let actual_subjects = stream
            .info()
            .await
            .context("failed to inspect NATS event stream")?
            .config
            .subjects
            .clone();
        if !actual_subjects
            .iter()
            .any(|subject| subject == &stream_subject)
        {
            bail!(
                "NATS stream {} does not capture {}; configured subjects: {}",
                config.stream_name,
                stream_subject,
                actual_subjects.join(", ")
            );
        }

        let (publisher, receiver) = mpsc::channel(config.publish_queue_capacity);
        tokio::spawn(run_nats_publisher(
            jetstream,
            config.subject_prefix.clone(),
            receiver,
        ));
        let (local, _) = broadcast::channel(LOCAL_SUBSCRIBER_CAPACITY);
        info!(
            stream = %config.stream_name,
            subjects = %stream_subject,
            "NATS event bus ready"
        );
        Ok(Self {
            local,
            nats: Some(publisher),
        })
    }

    pub fn publish(&self, event: DomainEventEnvelope) -> Result<(), EventBusPublishError> {
        let _ = self.local.send(event.clone());
        let Some(nats) = &self.nats else {
            return Ok(());
        };
        nats.try_send(event).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => EventBusPublishError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => EventBusPublishError::PublisherStopped,
        })
    }

    #[cfg(test)]
    pub fn subscribe(&self) -> broadcast::Receiver<DomainEventEnvelope> {
        self.local.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::in_process()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventBusPublishError {
    QueueFull,
    PublisherStopped,
}

impl fmt::Display for EventBusPublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull => formatter.write_str("NATS event publish queue is full"),
            Self::PublisherStopped => formatter.write_str("NATS event publisher has stopped"),
        }
    }
}

async fn run_nats_publisher(
    jetstream: jetstream::Context,
    subject_prefix: String,
    mut receiver: mpsc::Receiver<DomainEventEnvelope>,
) {
    while let Some(event) = receiver.recv().await {
        let subject = event_subject(&subject_prefix, &event.workspace_id, &event.action);
        let event_id = event.event_id.clone();
        let payload = match serde_json::to_vec(&event) {
            Ok(payload) => payload,
            Err(error) => {
                warn!(%event_id, %error, "failed to encode domain event");
                continue;
            }
        };
        let mut retry_delay = Duration::from_millis(250);
        loop {
            let publish = PublishMessage::build()
                .payload(payload.clone().into())
                .message_id(&event_id);
            let result = tokio::time::timeout(PUBLISH_TIMEOUT, async {
                jetstream
                    .send_publish(subject.clone(), publish)
                    .await
                    .context("failed to send event")?
                    .await
                    .context("failed to persist event")?;
                Ok::<_, anyhow::Error>(())
            })
            .await;
            match result {
                Ok(Ok(())) => break,
                Ok(Err(error)) => {
                    warn!(%event_id, %subject, %error, ?retry_delay, "NATS event publish failed; retrying");
                }
                Err(_) => {
                    warn!(%event_id, %subject, ?retry_delay, "NATS event publish timed out; retrying");
                }
            }
            tokio::time::sleep(retry_delay).await;
            retry_delay = retry_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
        }
    }
}

fn event_subject(prefix: &str, workspace_id: &str, action: &str) -> String {
    let workspace = URL_SAFE_NO_PAD.encode(workspace_id.as_bytes());
    let action = if action
        .split('.')
        .all(|token| !token.is_empty() && token.bytes().all(is_plain_subject_byte))
    {
        action.to_string()
    } else {
        format!("encoded_{}", URL_SAFE_NO_PAD.encode(action.as_bytes()))
    };
    format!("{prefix}.workspace_{workspace}.{action}")
}

fn validate_subject_prefix(prefix: &str) -> Result<()> {
    if prefix.is_empty()
        || !prefix
            .split('.')
            .all(|token| !token.is_empty() && token.bytes().all(is_plain_subject_byte))
    {
        bail!("NATS subject prefix must contain only non-empty alphanumeric, '-', or '_' tokens");
    }
    Ok(())
}

fn is_plain_subject_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use treer_protocol::{
        DomainEventActor, DomainEventEnvelope, DomainEventResource, DOMAIN_EVENT_SCHEMA_VERSION,
    };
    use uuid::Uuid;

    use super::*;

    #[test]
    fn subject_encodes_workspace_and_preserves_structured_action() {
        let subject = event_subject("treer.v1.events", "org/team alpha", "agent.updated");
        assert_eq!(
            subject,
            "treer.v1.events.workspace_b3JnL3RlYW0gYWxwaGE.agent.updated"
        );
        assert!(!subject.contains('*'));
        assert!(!subject.contains('>'));
        assert!(!subject.contains(char::is_whitespace));
    }

    #[test]
    fn arbitrary_actions_cannot_inject_subject_wildcards() {
        let subject = event_subject("treer.v1.events", "default", "agent.*.updated");
        assert!(subject.contains(".encoded_"));
        assert!(!subject.contains('*'));
        assert!(!subject.contains('>'));
    }

    #[test]
    fn invalid_subject_prefix_is_rejected() {
        assert!(validate_subject_prefix("treer.v1.events").is_ok());
        assert!(validate_subject_prefix("treer..events").is_err());
        assert!(validate_subject_prefix("treer.>").is_err());
    }

    #[tokio::test]
    async fn in_process_bus_delivers_the_shared_envelope() {
        let bus = EventBus::in_process();
        let mut receiver = bus.subscribe();
        let event = DomainEventEnvelope {
            event_id: "evt_test".to_string(),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            organization_id: None,
            workspace_id: "default".to_string(),
            actor: DomainEventActor {
                kind: "system".to_string(),
                id: Some("treer-proxy".to_string()),
            },
            action: "agent.updated".to_string(),
            resource: DomainEventResource {
                kind: "workspace".to_string(),
                id: "default".to_string(),
            },
            occurred_at: Utc::now(),
            trace_id: None,
            causation_id: None,
            correlation_id: None,
            workspace_revision: Some(3),
            payload: json!({"agent_id": "agent-1"}),
        };

        bus.publish(event.clone()).expect("publish");
        assert_eq!(receiver.recv().await.expect("event"), event);
    }

    #[tokio::test]
    async fn configured_nats_persists_and_deduplicates_events() {
        let Ok(nats_url) = std::env::var("TREER_TEST_NATS_URL") else {
            return;
        };
        let suffix = Uuid::new_v4().simple().to_string();
        let stream_name = format!("TREER_TEST_{}", suffix.to_ascii_uppercase());
        let subject_prefix = format!("treer.test.{suffix}");
        let bus = EventBus::connect_nats(EventBusConfig::new(
            nats_url.clone(),
            stream_name.clone(),
            subject_prefix.clone(),
        ))
        .await
        .expect("connect NATS event bus");
        let event = DomainEventEnvelope {
            event_id: format!("evt_{suffix}"),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            organization_id: None,
            workspace_id: "workspace with spaces".to_string(),
            actor: DomainEventActor {
                kind: "system".to_string(),
                id: Some("integration-test".to_string()),
            },
            action: "server.updated".to_string(),
            resource: DomainEventResource {
                kind: "workspace".to_string(),
                id: "workspace with spaces".to_string(),
            },
            occurred_at: Utc::now(),
            trace_id: Some("trace-test".to_string()),
            causation_id: None,
            correlation_id: Some("correlation-test".to_string()),
            workspace_revision: Some(7),
            payload: json!({"server_id": "server-1"}),
        };

        bus.publish(event.clone()).expect("first publish");
        bus.publish(event.clone()).expect("duplicate publish");

        let client = async_nats::connect(&nats_url).await.expect("NATS client");
        let context = jetstream::new(client);
        let mut stream = context.get_stream(&stream_name).await.expect("test stream");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let sequence = loop {
            let info = stream.info().await.expect("stream info");
            if info.state.messages == 1 {
                break info.state.last_sequence;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "event was not persisted in time"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            stream
                .info()
                .await
                .expect("deduplicated stream info")
                .state
                .messages,
            1
        );
        let stored = stream
            .get_raw_message(sequence)
            .await
            .expect("stored event");
        let decoded: DomainEventEnvelope =
            serde_json::from_slice(&stored.payload).expect("decode stored event");
        assert_eq!(decoded, event);
        assert_eq!(
            stored.subject.as_str(),
            event_subject(&subject_prefix, &event.workspace_id, &event.action)
        );
        context
            .delete_stream(&stream_name)
            .await
            .expect("delete test stream");
    }
}
