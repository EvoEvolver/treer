//! Explicit live-owner transfer contract; durable bytes are denial evidence,
//! never a constructor for a new owner, release permission or retry.
use crate::windows_update_intent::Binding;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use treer_protocol::{
    controller_update_worker::WorkerBindingFrame, controller_update_worker_inputs::Snapshot,
};

pub(crate) const LIMIT: usize = 4096;
pub(crate) const LIFETIME_SECONDS: i64 = 120;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transfer {
    schema_version: u32,
    phase: String,
    binding: Binding,
    operation: [u8; 16],
    challenge: [u8; 16],
    offer_sha256: [u8; 32],
    snapshot: Option<Snapshot>,
    expires_at_ms: i64,
    // Local monotonic bound, never accepted from serialized record bytes.
    #[serde(skip)]
    deadline: Option<tokio::time::Instant>,
}
impl Transfer {
    pub(crate) fn new(
        binding: &Binding,
        context: WorkerBindingFrame,
        snapshot: Option<Snapshot>,
        now: i64,
    ) -> Self {
        Self {
            schema_version: 1,
            phase: "receiver_custody_accepted".into(),
            binding: binding.clone(),
            operation: context.operation,
            challenge: context.challenge,
            offer_sha256: context.context_sha256,
            snapshot,
            expires_at_ms: now + LIFETIME_SECONDS * 1000,
            deadline: Some(
                tokio::time::Instant::now()
                    + std::time::Duration::from_secs(LIFETIME_SECONDS as u64),
            ),
        }
    }
    pub(crate) fn check(
        &self,
        binding: &Binding,
        context: WorkerBindingFrame,
        snapshot: &Option<Snapshot>,
        now: i64,
    ) -> Result<()> {
        binding.validate()?;
        if self.schema_version != 1
            || self.phase != "receiver_custody_accepted"
            || self.binding != *binding
            || self.operation != *binding.operation_id.as_bytes()
            || self.operation != context.operation
            || self.challenge != context.challenge
            || self.offer_sha256 != context.context_sha256
            || self.snapshot != *snapshot
            || self.expires_at_ms <= now
            || self.expires_at_ms > now + LIFETIME_SECONDS * 1000
        {
            bail!("windows_update_transfer_unconfirmed");
        }
        Ok(())
    }
    pub(crate) fn remaining(&self, now: i64) -> Result<std::time::Duration> {
        let remaining = self
            .expires_at_ms
            .checked_sub(now)
            .filter(|n| *n > 0 && *n <= LIFETIME_SECONDS * 1000)
            .ok_or_else(|| anyhow::anyhow!("windows_update_transfer_expired"))?;
        let wall = std::time::Duration::from_millis(remaining as u64);
        match self.deadline {
            Some(deadline) => Ok(wall.min(
                deadline
                    .checked_duration_since(tokio::time::Instant::now())
                    .ok_or_else(|| anyhow::anyhow!("windows_update_transfer_expired"))?,
            )),
            None => Ok(wall),
        }
    }
    pub(crate) fn arm_receiver_lifetime(&mut self, now: i64) -> Result<()> {
        if self.deadline.is_some() {
            bail!("windows_update_transfer_consumed");
        }
        self.deadline = Some(tokio::time::Instant::now() + self.remaining(now)?);
        Ok(())
    }
    pub(crate) fn retention_deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let raw = serde_json::to_vec(self)?;
        if raw.len() > LIMIT {
            bail!("windows_update_transfer_unconfirmed");
        }
        Ok(raw)
    }
    pub(crate) fn decode(raw: &[u8]) -> Result<Self> {
        if raw.is_empty() || raw.len() > LIMIT {
            bail!("windows_update_transfer_unconfirmed");
        }
        Ok(serde_json::from_slice(raw)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Binding, WorkerBindingFrame) {
        let operation = uuid::Uuid::new_v4();
        (
            Binding {
                operation_id: operation,
                workspace_id: "ws_transfer".into(),
                server_id: "srv_transfer".into(),
                installation_volume: 1,
                installation_file_id: 2,
                old_controller_sha256: "1".repeat(64),
                old_cli_sha256: "2".repeat(64),
                controller_sha256: "3".repeat(64),
                cli_sha256: "4".repeat(64),
                signed_manifest_sha256: "5".repeat(64),
            },
            WorkerBindingFrame {
                operation: *operation.as_bytes(),
                challenge: *uuid::Uuid::new_v4().as_bytes(),
                context_sha256: [4; 32],
            },
        )
    }
    #[test]
    fn windows_update_transfer_exact_context_state_and_bounded_lifetime() {
        let (binding, context) = fixture();
        let transfer = Transfer::new(&binding, context, None, 1000);
        let raw = transfer.encode().unwrap();
        let decoded = Transfer::decode(&raw).unwrap();
        decoded.check(&binding, context, &None, 1001).unwrap();
        assert_eq!(decoded.remaining(1001).unwrap().as_millis(), 119999);
        let mut armed = Transfer::decode(&raw).unwrap();
        armed.arm_receiver_lifetime(1001).unwrap();
        let deadline = armed.retention_deadline().unwrap();
        assert!(armed.arm_receiver_lifetime(1002).is_err());
        assert_eq!(armed.retention_deadline(), Some(deadline));
        armed.deadline = Some(tokio::time::Instant::now() - std::time::Duration::from_millis(1));
        assert!(
            armed.remaining(1001).is_err(),
            "valid wall time cannot extend expired local custody"
        );
        assert!(decoded.remaining(121000).is_err());
        assert!(decoded.remaining(0).is_err());
        let mut foreign = context;
        foreign.challenge = *uuid::Uuid::new_v4().as_bytes();
        assert!(decoded.check(&binding, foreign, &None, 1001).is_err());
        let mut other = binding.clone();
        other.server_id = "srv_other".into();
        assert!(decoded.check(&other, context, &None, 1001).is_err());
        assert!(decoded
            .check(
                &binding,
                context,
                &Some(Snapshot {
                    file_id: 1,
                    size: 1,
                    sha256: "1".repeat(64)
                }),
                1001
            )
            .is_err());
        let mut extra: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        extra["release"] = true.into();
        assert!(Transfer::decode(&serde_json::to_vec(&extra).unwrap()).is_err());
        assert!(Transfer::decode(&vec![0; LIMIT + 1]).is_err());
    }
}
