//! Core authenticates sender IDs; these envelopes provide framing, not identity.
//! A daemon must dispatch on the outer Core sender before parsing control text.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

pub const MAX_BODY: usize = 32_768;
pub const MAX_ENVELOPE: usize = 256 * 1024;
pub const PART_BYTES: usize = 18_000;
pub const MAX_PARTS: usize = 16;
const PROTOCOL: &str = "treer.endpoint/v1";

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid message envelope")]
    Invalid,
    #[error("message sender unavailable")]
    Sender,
    #[error("recipient unavailable")]
    Recipient,
    #[error("fragment conflict")]
    Conflict,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Outbound,
    Inbound,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    pub protocol: String,
    pub direction: Direction,
    pub generation: u64,
    pub transfer_id: String,
    pub payload_digest: String,
    pub part: u16,
    pub parts: u16,
    pub data: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sender {
    pub kind: SenderKind,
    pub id: String,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderKind {
    Human,
    Agent,
    Service,
    Machine,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Envelope {
    Outbound {
        delivery_id: String,
        message_id: String,
        original_sender: Sender,
        body: String,
        context_ids: Vec<String>,
    },
    Deliver {
        job_id: String,
        recipient: String,
        body: String,
        context_ids: Vec<String>,
        source: ExternalSource,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSource {
    pub channel: Channel,
    pub account_id: String,
    pub conversation_id: String,
    pub message_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Telegram,
    Email,
    Http,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub(crate) fn digest_id(label: &str, endpoint: &str, generation: u64, source: &str) -> String {
    // Length-prefix IDs to avoid delimiter ambiguity.
    let mut hash = Sha256::new();
    for value in [label, endpoint, &generation.to_string(), source] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl Envelope {
    pub fn source_id(&self) -> &str {
        match self {
            Self::Outbound { delivery_id, .. } => delivery_id,
            Self::Deliver { job_id, .. } => job_id,
        }
    }
    pub fn direction(&self) -> Direction {
        match self {
            Self::Outbound { .. } => Direction::Outbound,
            Self::Deliver { .. } => Direction::Inbound,
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        let (body, contexts, ids) = match self {
            Self::Outbound {
                delivery_id,
                message_id,
                original_sender,
                body,
                context_ids,
            } => (
                body,
                context_ids,
                vec![delivery_id, message_id, &original_sender.id],
            ),
            Self::Deliver {
                job_id,
                recipient,
                body,
                context_ids,
                source,
            } => (
                body,
                context_ids,
                vec![
                    job_id,
                    recipient,
                    &source.account_id,
                    &source.conversation_id,
                    &source.message_id,
                ],
            ),
        };
        if body.trim().is_empty()
            || body.len() > MAX_BODY
            || contexts.len() > 32
            || !contexts.iter().all(|id| valid_id(id))
            || !ids.iter().all(|id| valid_id(id))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    /// Splitting handles wrapper escaping even when original body already fills
    /// Core's 32KiB limit. Persist/retry the exact returned part bytes.
    pub fn fragments(&self, endpoint: &str, generation: u64) -> Result<Vec<String>, Error> {
        self.validate()?;
        let source = self.source_id();
        if !valid_id(endpoint) || !valid_id(source) || generation == 0 {
            return Err(Error::Invalid);
        }
        let payload = serde_json::to_vec(self).map_err(|_| Error::Invalid)?;
        if payload.len() > MAX_ENVELOPE {
            return Err(Error::Invalid);
        }
        let direction = self.direction();
        let label = if direction == Direction::Outbound {
            "outbound"
        } else {
            "inbound"
        };
        let transfer = digest_id(label, endpoint, generation, source);
        let hash = digest(&payload);
        let count = payload.len().div_ceil(PART_BYTES);
        if count > MAX_PARTS {
            return Err(Error::Invalid);
        }
        payload
            .chunks(PART_BYTES)
            .enumerate()
            .map(|(index, chunk)| {
                let frame = Fragment {
                    protocol: PROTOCOL.into(),
                    direction,
                    generation,
                    transfer_id: transfer.clone(),
                    payload_digest: hash.clone(),
                    part: index as u16,
                    parts: count as u16,
                    data: STANDARD.encode(chunk),
                };
                let text = serde_json::to_string(&frame).map_err(|_| Error::Invalid)?;
                if text.len() > MAX_BODY {
                    return Err(Error::Invalid);
                }
                Ok(text)
            })
            .collect()
    }
}

impl Fragment {
    pub fn parse(text: &str) -> Result<Self, Error> {
        if text.is_empty() || text.len() > MAX_BODY {
            return Err(Error::Invalid);
        }
        let frame: Self = serde_json::from_str(text).map_err(|_| Error::Invalid)?;
        if frame.protocol != PROTOCOL
            || frame.generation == 0
            || frame.parts == 0
            || usize::from(frame.parts) > MAX_PARTS
            || frame.part >= frame.parts
            || !hex_digest(&frame.transfer_id)
            || !hex_digest(&frame.payload_digest)
            || frame.data.len() > PART_BYTES.div_ceil(3) * 4
        {
            return Err(Error::Invalid);
        }
        let raw = STANDARD.decode(&frame.data).map_err(|_| Error::Invalid)?;
        if raw.is_empty() || raw.len() > PART_BYTES {
            return Err(Error::Invalid);
        }
        Ok(frame)
    }
    pub fn idempotency_key(&self) -> String {
        format!("ep-{}-{}", self.transfer_id, self.part)
    }
}
fn hex_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One bounded transfer assembly. Caller also caps number/total bytes of
/// simultaneous assemblies and persists hub parts before acknowledging Core.
pub struct Assembly {
    endpoint: String,
    transfer: String,
    hash: String,
    generation: u64,
    direction: Direction,
    count: u16,
    bytes: usize,
    parts: BTreeMap<u16, Vec<u8>>,
}
impl Assembly {
    pub fn new(
        first: &Fragment,
        endpoint: &str,
        generation: u64,
        direction: Direction,
    ) -> Result<Self, Error> {
        if !valid_id(endpoint)
            || generation == 0
            || first.generation != generation
            || first.direction != direction
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            endpoint: endpoint.into(),
            transfer: first.transfer_id.clone(),
            hash: first.payload_digest.clone(),
            generation,
            direction,
            count: first.parts,
            bytes: 0,
            parts: BTreeMap::new(),
        })
    }
    pub fn push(&mut self, frame: &Fragment) -> Result<Option<Envelope>, Error> {
        // Validate even frames constructed directly rather than through parse.
        let text = serde_json::to_string(frame).map_err(|_| Error::Invalid)?;
        let frame = Fragment::parse(&text)?;
        if frame.transfer_id != self.transfer
            || frame.payload_digest != self.hash
            || frame.generation != self.generation
            || frame.direction != self.direction
            || frame.parts != self.count
        {
            return Err(Error::Conflict);
        }
        let bytes = STANDARD.decode(&frame.data).map_err(|_| Error::Invalid)?;
        if let Some(previous) = self.parts.get(&frame.part) {
            if *previous != bytes {
                return Err(Error::Conflict);
            }
        } else {
            if self.bytes + bytes.len() > MAX_ENVELOPE {
                return Err(Error::Invalid);
            }
            self.bytes += bytes.len();
            self.parts.insert(frame.part, bytes);
        }
        if self.parts.len() != usize::from(self.count) {
            return Ok(None);
        }
        let mut complete = Vec::with_capacity(self.bytes);
        for part in self.parts.values() {
            complete.extend_from_slice(part);
        }
        if digest(&complete) != self.hash {
            return Err(Error::Conflict);
        }
        let envelope: Envelope = serde_json::from_slice(&complete).map_err(|_| Error::Invalid)?;
        envelope.validate()?;
        let label = if self.direction == Direction::Outbound {
            "outbound"
        } else {
            "inbound"
        };
        let expected = digest_id(label, &self.endpoint, self.generation, envelope.source_id());
        if envelope.direction() != self.direction || expected != self.transfer {
            return Err(Error::Conflict);
        }
        Ok(Some(envelope))
    }
}

/// Trusted sender comes ONLY from the outer authenticated Core delivery, never
/// from envelope fields. Non-hub text remains ordinary outbound, even if JSON.
pub fn from_hub(sender: &Sender, pinned_hub: &str) -> bool {
    sender.kind == SenderKind::Agent && sender.id == pinned_hub
}

pub fn check_recipient(envelope: &Envelope, local_allowlist: &[String]) -> Result<(), Error> {
    match envelope {
        Envelope::Deliver { recipient, .. } if local_allowlist.contains(recipient) => Ok(()),
        _ => Err(Error::Recipient),
    }
}
