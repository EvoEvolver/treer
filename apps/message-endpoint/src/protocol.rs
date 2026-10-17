//! Bounded framing and mutually authenticated, direction-specific sessions.
//!
//! Callers must provide CSPRNG nonces, reject expired handshakes, check peer UID
//! and private file/socket ownership, and recheck binding generation per operation.
//! The pure protocol deliberately performs no filesystem or Treer operations.
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::io::{Read, Write};
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;
pub const MAX_FRAME: usize = 128 * 1024;
pub const MAX_BODY: usize = 60_000;
const MAX_ID: usize = 256;
const DOMAIN: &[u8] = b"treer.endpoint.v1";

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid protocol frame")]
    Invalid,
    #[error("authentication failed")]
    Authentication,
    #[error("protocol connection unavailable")]
    Io(#[source] std::io::Error),
}

/// Binding keys intentionally do not implement Debug, Clone or Serialize.
pub struct BindingKey([u8; 32]);
impl BindingKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Nonces must be independently random for every connection, including retries.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    pub version: u32,
    pub endpoint_id: String,
    pub generation: u64,
    pub hub_nonce: [u8; 32],
    pub endpoint_nonce: [u8; 32],
}
impl Transcript {
    fn bytes(&self) -> Result<Vec<u8>, Error> {
        if self.version != 1 || self.generation == 0 || !valid_id(&self.endpoint_id) {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::with_capacity(350);
        // Length-prefix all variable fields; use network byte order for integers.
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(&(self.endpoint_id.len() as u32).to_be_bytes());
        bytes.extend_from_slice(self.endpoint_id.as_bytes());
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        bytes.extend_from_slice(&self.hub_nonce);
        bytes.extend_from_slice(&self.endpoint_nonce);
        Ok(bytes)
    }
}

#[derive(Clone, Copy)]
pub enum Role {
    Endpoint,
    Hub,
}
impl Role {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Endpoint => b"endpoint-proof",
            Self::Hub => b"hub-proof",
        }
    }
}

fn mac(key: &[u8], label: &[u8], data: &[u8]) -> HmacSha256 {
    // HMAC accepts arbitrary key lengths, including the fixed 32-byte keys here.
    let mut result = HmacSha256::new_from_slice(key).expect("HMAC accepts 32-byte keys");
    result.update(DOMAIN);
    result.update(&(label.len() as u32).to_be_bytes());
    result.update(label);
    result.update(data);
    result
}

pub fn proof(key: &BindingKey, role: Role, transcript: &Transcript) -> Result<[u8; 32], Error> {
    Ok(mac(&key.0, role.label(), &transcript.bytes()?)
        .finalize()
        .into_bytes()
        .into())
}

pub fn verify_proof(
    key: &BindingKey,
    role: Role,
    transcript: &Transcript,
    received: &[u8; 32],
) -> Result<(), Error> {
    mac(&key.0, role.label(), &transcript.bytes()?)
        .verify_slice(received)
        .map_err(|_| Error::Authentication)
}

/// Construct only after verifying the opposite side's proof. Callers own the
/// 5-second challenge lifetime and one-use handshake state.
pub struct Session {
    send_key: [u8; 32],
    receive_key: [u8; 32],
    send_sequence: u64,
    receive_sequence: u64,
    failed: bool,
}
impl Session {
    pub fn authenticated(
        key: &BindingKey,
        role: Role,
        transcript: &Transcript,
        remote_proof: &[u8; 32],
    ) -> Result<Self, Error> {
        let opposite = match role {
            Role::Endpoint => Role::Hub,
            Role::Hub => Role::Endpoint,
        };
        verify_proof(key, opposite, transcript, remote_proof)?;
        let bytes = transcript.bytes()?;
        let endpoint_key: [u8; 32] = mac(&key.0, b"endpoint-frame-key", &bytes)
            .finalize()
            .into_bytes()
            .into();
        let hub_key: [u8; 32] = mac(&key.0, b"hub-frame-key", &bytes)
            .finalize()
            .into_bytes()
            .into();
        let (send_key, receive_key) = match role {
            Role::Endpoint => (endpoint_key, hub_key),
            Role::Hub => (hub_key, endpoint_key),
        };
        Ok(Self {
            send_key,
            receive_key,
            send_sequence: 0,
            receive_sequence: 0,
            failed: false,
        })
    }

    pub fn seal(&mut self, payload: &[u8]) -> Result<Vec<u8>, Error> {
        if self.failed || payload.is_empty() || payload.len() > MAX_FRAME - 40 {
            self.failed = true;
            return Err(Error::Invalid);
        }
        let Some(next) = self.send_sequence.checked_add(1) else {
            self.failed = true;
            return Err(Error::Invalid);
        };
        let mut bytes = Vec::with_capacity(payload.len() + 40);
        bytes.extend_from_slice(&next.to_be_bytes());
        bytes.extend_from_slice(payload);
        let tag = mac(&self.send_key, b"frame", &bytes)
            .finalize()
            .into_bytes();
        bytes.extend_from_slice(&tag);
        self.send_sequence = next;
        Ok(bytes)
    }

    pub fn open<'a>(&mut self, frame: &'a [u8]) -> Result<&'a [u8], Error> {
        let result = self.open_inner(frame);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn open_inner<'a>(&mut self, frame: &'a [u8]) -> Result<&'a [u8], Error> {
        if self.failed || frame.len() <= 40 || frame.len() > MAX_FRAME {
            return Err(Error::Invalid);
        }
        let sequence = u64::from_be_bytes(frame[..8].try_into().map_err(|_| Error::Invalid)?);
        if Some(sequence) != self.receive_sequence.checked_add(1) {
            return Err(Error::Authentication);
        }
        let end = frame.len() - 32;
        mac(&self.receive_key, b"frame", &frame[..end])
            .verify_slice(&frame[end..])
            .map_err(|_| Error::Authentication)?;
        self.receive_sequence = sequence;
        Ok(&frame[8..end])
    }
}

/// The wire frame length is bounded before allocating payload storage. Transport
/// callers must set read/write deadlines; generic Read/Write cannot enforce them.
pub fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>, Error> {
    let mut header = [0; 4];
    reader.read_exact(&mut header).map_err(Error::Io)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(Error::Invalid);
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).map_err(Error::Io)?;
    Ok(bytes)
}

pub fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> Result<(), Error> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(Error::Invalid);
    }
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|()| writer.write_all(bytes))
        .map_err(Error::Io)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && !id.chars().any(char::is_control)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sender {
    pub kind: SenderKind,
    pub id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SenderKind {
    Human,
    Agent,
    Service,
}

/// Exact tagged variants reject unknown fields and duplicate recognized fields;
/// no generic JSON maps allow hidden identity/target overrides.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Submit {
        request_id: String,
        delivery_id: String,
        message_id: String,
        sender: Sender,
        body: String,
        context_ids: Vec<String>,
        created_at: String,
    },
    Pull {
        request_id: String,
        wait_ms: u32,
    },
    Complete {
        request_id: String,
        job_id: String,
        lease_token: String,
        core_message_id: String,
    },
    Defer {
        request_id: String,
        job_id: String,
        lease_token: String,
        reason: DeferReason,
    },
    Status {
        request_id: String,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferReason {
    CoreUnavailable,
    CoreRejected,
    AmbiguousSend,
}

impl Request {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > MAX_FRAME - 40 {
            return Err(Error::Invalid);
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
        let (request_id, valid) = match &request {
            Self::Submit {
                request_id,
                delivery_id,
                message_id,
                sender,
                body,
                context_ids,
                created_at,
            } => (
                request_id,
                valid_id(delivery_id)
                    && valid_id(message_id)
                    && valid_id(&sender.id)
                    && body.len() <= MAX_BODY
                    && context_ids.len() <= 32
                    && context_ids.iter().all(|id| valid_id(id))
                    && valid_id(created_at),
            ),
            Self::Pull {
                request_id,
                wait_ms,
            } => (request_id, *wait_ms <= 250),
            Self::Complete {
                request_id,
                job_id,
                lease_token,
                core_message_id,
            } => (
                request_id,
                valid_id(job_id) && valid_id(lease_token) && valid_id(core_message_id),
            ),
            Self::Defer {
                request_id,
                job_id,
                lease_token,
                ..
            } => (request_id, valid_id(job_id) && valid_id(lease_token)),
            Self::Status { request_id } => (request_id, true),
        };
        if !valid_id(request_id) || !valid {
            return Err(Error::Invalid);
        }
        Ok(request)
    }
}
