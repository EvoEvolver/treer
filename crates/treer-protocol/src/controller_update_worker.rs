//! Fixed bootstrap agreement only, never an update command or readiness proof.
//! Runtime/installation authority stays with independently retained Core state.

pub const WORKER_BINDING_FRAME_BYTES: usize = 72;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerBindingKind {
    Request,
    Ack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerBindingFrame {
    pub operation: [u8; 16],
    pub context_sha256: [u8; 32],
    pub challenge: [u8; 16],
}

impl WorkerBindingFrame {
    fn valid(&self) -> bool {
        // Canonical UUIDv4/RFC variant without a UUID dependency in wire models.
        [self.operation, self.challenge]
            .iter()
            .all(|id| id[6] >> 4 == 4 && id[8] >> 6 == 2)
    }

    pub fn encode(&self, kind: WorkerBindingKind) -> Option<[u8; WORKER_BINDING_FRAME_BYTES]> {
        if !self.valid() {
            return None;
        }
        let mut bytes = [0; WORKER_BINDING_FRAME_BYTES];
        bytes[..8].copy_from_slice(match kind {
            WorkerBindingKind::Request => b"TRWREQ01",
            WorkerBindingKind::Ack => b"TRWACK01",
        });
        bytes[8..24].copy_from_slice(&self.operation);
        bytes[24..56].copy_from_slice(&self.context_sha256);
        bytes[56..].copy_from_slice(&self.challenge);
        Some(bytes)
    }

    pub fn decode(bytes: &[u8], kind: WorkerBindingKind) -> Option<Self> {
        if bytes.len() != WORKER_BINDING_FRAME_BYTES {
            return None;
        }
        let frame = Self {
            operation: bytes[8..24].try_into().ok()?,
            context_sha256: bytes[24..56].try_into().ok()?,
            challenge: bytes[56..72].try_into().ok()?,
        };
        // Checks role/version, UUIDs, and the entire fixed encoding.
        (frame.encode(kind)?.as_slice() == bytes).then_some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_binding_frames_reject_wrong_role_version_bounds_and_uuid() {
        let mut id = [1; 16];
        id[6] = 0x41;
        id[8] = 0x81;
        let frame = WorkerBindingFrame {
            operation: id,
            context_sha256: [2; 32],
            challenge: id,
        };
        let bytes = frame.encode(WorkerBindingKind::Request).unwrap();
        assert_eq!(
            WorkerBindingFrame::decode(&bytes, WorkerBindingKind::Request),
            Some(frame)
        );
        assert!(WorkerBindingFrame::decode(&bytes, WorkerBindingKind::Ack).is_none());
        assert!(WorkerBindingFrame::decode(&bytes[..71], WorkerBindingKind::Request).is_none());
        let mut extra = bytes.to_vec();
        extra.push(0);
        assert!(WorkerBindingFrame::decode(&extra, WorkerBindingKind::Request).is_none());
        for offset in [7, 14, 16, 62, 64] {
            let mut invalid = bytes;
            invalid[offset] = 0;
            assert!(WorkerBindingFrame::decode(&invalid, WorkerBindingKind::Request).is_none());
        }
    }
}
