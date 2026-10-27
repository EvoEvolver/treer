//! Bounded receiver input observations. Neither paths, IDs, signatures nor a
//! received receipt authorize an update or release of original sender custody.
use serde::{Deserialize, Serialize};

use crate::{controller_update_runtime::DispatchBinding, controller_update_worker::*};

pub const INPUT_HEADER_BYTES: usize = 12;
pub const INPUT_LIMIT: usize = 512 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairLocation {
    pub directory: String,
    pub identities: [(u64, u64); 3],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateLocation {
    pub directory: String,
    pub identity: (u64, u64),
    pub snapshot: Option<Snapshot>,
}

/// Locators and a reduced-rights handle value local to the original Child.
/// Neither deserialization nor identity equality grants transaction authority.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobLocation {
    pub directory: String,
    pub identity: (u64, u64),
    pub lock: (u64, u64),
    pub child_handle: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub file_id: u64,
    pub size: u64,
    pub sha256: String,
}

/// Only a retained selected-runtime sender may mint the trusted offer. A
/// deserialized value is still data: receiver trust and policy are separate.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputOffer {
    pub schema_version: u32,
    pub operation: [u8; 16],
    pub challenge: [u8; 16],
    pub runtime_context_sha256: [u8; 32],
    pub dispatch: DispatchBinding,
    pub signed_manifest_sha256: String,
    pub lock: (u64, u64),
    pub installed: PairLocation,
    pub target: PairLocation,
    pub state: StateLocation,
    pub job: JobLocation,
    pub channel_name: String,
    pub channel: Vec<u8>,
    pub channel_signature: Vec<u8>,
    pub manifest: Vec<u8>,
    pub manifest_signature: Vec<u8>,
}

impl InputOffer {
    pub fn valid_shape(&self) -> bool {
        let hash = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let path = |s: &str| !s.is_empty() && s.len() <= 32768 && !s.contains('\0');
        self.schema_version == 1
            && WorkerBindingFrame {
                operation: self.operation,
                context_sha256: self.runtime_context_sha256,
                challenge: self.challenge,
            }
            .encode(WorkerBindingKind::Request)
            .is_some()
            && self.dispatch.validate()
            && hash(&self.signed_manifest_sha256)
            && self.lock.1 != 0
            && [&self.installed, &self.target].iter().all(|p| {
                path(&p.directory)
                    && p.identities
                        .iter()
                        .all(|id| id.1 != 0 && id.0 == self.lock.0)
            })
            && path(&self.state.directory)
            && self.state.identity.1 != 0
            && self.state.identity.0 == self.lock.0
            && self.state.snapshot.as_ref().is_none_or(|s| {
                s.file_id != 0 && s.size > 0 && s.size <= 16 * 1024 * 1024 && hash(&s.sha256)
            })
            && path(&self.job.directory)
            && self.job.identity.1 != 0
            && self.job.identity.0 == self.lock.0
            && self.job.lock.1 != 0
            && self.job.lock.0 == self.lock.0
            && self.job.child_handle != 0
            && self.job.child_handle != u64::MAX
            && !self.channel_name.is_empty()
            && self.channel_name.len() <= 32
            && !self.channel.is_empty()
            && self.channel.len() <= 64 * 1024
            && !self.manifest.is_empty()
            && self.manifest.len() <= 128 * 1024
            && [&self.channel_signature, &self.manifest_signature]
                .iter()
                .all(|s| !s.is_empty() && s.len() <= 2048)
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid_shape() {
            return None;
        }
        let body = serde_json::to_vec(self).ok()?;
        if body.is_empty() || body.len() > INPUT_LIMIT {
            return None;
        }
        let mut bytes = b"TRWIRF01".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend(body);
        Some(bytes)
    }

    pub fn body_length(header: &[u8; INPUT_HEADER_BYTES]) -> Option<usize> {
        if &header[..8] != b"TRWIRF01" {
            return None;
        }
        let size = u32::from_le_bytes(header[8..12].try_into().ok()?) as usize;
        (size > 0 && size <= INPUT_LIMIT).then_some(size)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let header = bytes.get(..INPUT_HEADER_BYTES)?.try_into().ok()?;
        let size = Self::body_length(header)?;
        if bytes.len() != INPUT_HEADER_BYTES + size {
            return None;
        }
        let offer: Self = serde_json::from_slice(&bytes[INPUT_HEADER_BYTES..]).ok()?;
        offer.valid_shape().then_some(offer)
    }
}

/// Receipt of independently verified inputs, deliberately a distinct wire role
/// from bootstrap ACK. It conveys no permission to release sender pins.
pub struct InputReceipt(pub WorkerBindingFrame);
impl InputReceipt {
    pub fn encode(&self) -> Option<[u8; WORKER_BINDING_FRAME_BYTES]> {
        let mut bytes = self.0.encode(WorkerBindingKind::Ack)?;
        bytes[..8].copy_from_slice(b"TRWIRX01");
        Some(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != WORKER_BINDING_FRAME_BYTES || &bytes[..8] != b"TRWIRX01" {
            return None;
        }
        let mut legacy: [u8; WORKER_BINDING_FRAME_BYTES] = bytes.try_into().ok()?;
        legacy[..8].copy_from_slice(b"TRWACK01");
        Some(Self(WorkerBindingFrame::decode(
            &legacy,
            WorkerBindingKind::Ack,
        )?))
    }
}

/// Bounded custody control on the SAME received-input connection. Inspection
/// and close confer no write authority; PrepareBackup is a distinct, one-use
/// backup-only grant. No role permits sender release or installation changes.
/// Neither inspection nor close ACK permits sender release or an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustodyKind {
    Inspect,
    Inspected,
    Close,
    Closed,
    /// Explicit original-creator command for private backup preparation ONLY.
    PrepareBackup,
    BackupPrepared,
    /// Explicit acceptance of a loaded-store CAS on the SAME Holding session.
    StartupCommitted,
    StartupAccepted,
    /// Explicit fixed-candidate preparation only; installed names stay pinned.
    PrepareCandidates,
    CandidatesPrepared,
    /// Explicit bounded custody transfer; ACK alone is not sender release.
    TransferCustody,
    CustodyTransferred,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupTransition {
    pub previous: Option<Snapshot>,
    pub next: Option<Snapshot>,
}

pub const STARTUP_TRANSITION_LIMIT: usize = 512;

pub const CUSTODY_BYTES: usize = WORKER_BINDING_FRAME_BYTES + 4;
pub const CUSTODY_MAX_STEPS: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustodyFrame {
    pub context: WorkerBindingFrame,
    pub sequence: u32,
}
impl CustodyFrame {
    pub fn encode(&self, kind: CustodyKind) -> Option<[u8; CUSTODY_BYTES]> {
        if self.sequence == 0 || self.sequence > CUSTODY_MAX_STEPS {
            return None;
        }
        let mut bytes = [0; CUSTODY_BYTES];
        bytes[..WORKER_BINDING_FRAME_BYTES]
            .copy_from_slice(&self.context.encode(WorkerBindingKind::Request)?);
        bytes[..8].copy_from_slice(match kind {
            CustodyKind::Inspect => b"TRWCIN01",
            CustodyKind::Inspected => b"TRWCIA01",
            CustodyKind::Close => b"TRWCCL01",
            CustodyKind::Closed => b"TRWCCA01",
            CustodyKind::PrepareBackup => b"TRWCBP01",
            CustodyKind::BackupPrepared => b"TRWCBA01",
            CustodyKind::StartupCommitted => b"TRWCSC01",
            CustodyKind::StartupAccepted => b"TRWCSA01",
            CustodyKind::PrepareCandidates => b"TRWCNP01",
            CustodyKind::CandidatesPrepared => b"TRWCNA01",
            CustodyKind::TransferCustody => b"TRWCTF01",
            CustodyKind::CustodyTransferred => b"TRWCTA01",
        });
        bytes[WORKER_BINDING_FRAME_BYTES..].copy_from_slice(&self.sequence.to_le_bytes());
        Some(bytes)
    }
    pub fn decode(bytes: &[u8], kind: CustodyKind) -> Option<Self> {
        if bytes.len() != CUSTODY_BYTES {
            return None;
        }
        let mut legacy: [u8; WORKER_BINDING_FRAME_BYTES] =
            bytes[..WORKER_BINDING_FRAME_BYTES].try_into().ok()?;
        legacy[..8].copy_from_slice(b"TRWREQ01");
        let frame = Self {
            context: WorkerBindingFrame::decode(&legacy, WorkerBindingKind::Request)?,
            sequence: u32::from_le_bytes(bytes[WORKER_BINDING_FRAME_BYTES..].try_into().ok()?),
        };
        (frame.encode(kind)?.as_slice() == bytes).then_some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_update_coordinator_custody_wire_roles_sequence_and_bounds() {
        let mut id = [1; 16];
        id[6] = 0x41;
        id[8] = 0x81;
        let mut frame = CustodyFrame {
            context: WorkerBindingFrame {
                operation: id,
                challenge: id,
                context_sha256: [3; 32],
            },
            sequence: 1,
        };
        let roles = [
            CustodyKind::Inspect,
            CustodyKind::Inspected,
            CustodyKind::Close,
            CustodyKind::Closed,
        ];
        for role in roles {
            let bytes = frame.encode(role).unwrap();
            assert_eq!(CustodyFrame::decode(&bytes, role), Some(frame));
            for other in roles.into_iter().filter(|other| *other != role) {
                assert!(CustodyFrame::decode(&bytes, other).is_none());
            }
            assert!(InputReceipt::decode(&bytes).is_none());
            assert!(CustodyFrame::decode(&bytes[..CUSTODY_BYTES - 1], role).is_none());
            let mut extra = bytes.to_vec();
            extra.push(0);
            assert!(CustodyFrame::decode(&extra, role).is_none());
        }
        for sequence in [0, CUSTODY_MAX_STEPS + 1] {
            frame.sequence = sequence;
            assert!(frame.encode(CustodyKind::Inspect).is_none());
        }
        frame.sequence = CUSTODY_MAX_STEPS;
        assert!(frame.encode(CustodyKind::Close).is_some());
    }
    #[test]
    fn windows_update_worker_receive_wire_bounds_and_receipt_role() {
        let mut header = *b"TRWIRF01\x01\x00\x00\x00";
        assert_eq!(InputOffer::body_length(&header), Some(1));
        header[8..12].copy_from_slice(&((INPUT_LIMIT + 1) as u32).to_le_bytes());
        assert!(InputOffer::body_length(&header).is_none());
        header[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(InputOffer::body_length(&header).is_none());
        assert!(InputOffer::decode(b"TRWIRF01\x01\x00\x00\x00{}").is_none());
        let mut id = [1; 16];
        id[6] = 0x41;
        id[8] = 0x81;
        let frame = WorkerBindingFrame {
            operation: id,
            challenge: id,
            context_sha256: [3; 32],
        };
        let bytes = InputReceipt(frame).encode().unwrap();
        assert_eq!(InputReceipt::decode(&bytes).unwrap().0, frame);
        assert!(WorkerBindingFrame::decode(&bytes, WorkerBindingKind::Ack).is_none());
        assert!(InputReceipt::decode(&frame.encode(WorkerBindingKind::Ack).unwrap()).is_none());
    }
}
