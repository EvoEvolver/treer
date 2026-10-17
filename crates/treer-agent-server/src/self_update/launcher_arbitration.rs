//! Serialized in-memory permit arbitration only, NOT execution authority.
//! The supervisor must authenticate inputs, prove writer closure, inspect EOF
//! before closing its receiver, and durably register its identity separately.
//! No production lifecycle caller, process launch, ACK, noexec record or receipt
//! mutation is implemented here. Permit acceptance consumes a connection-bound
//! authenticated read; its metadata is not proof of durable registration. The
//! raw transport submodule reads framing only. Noexec transitions remain a test
//! model until authenticated writer-closure proofs exist.
use anyhow::{bail, Result};
use uuid::Uuid;

use super::hex;

pub mod transport;

/// Metadata binding to one unique attempt, exact intent bytes, and registered
/// supervisor identity record. Digests must come from independently verified
/// records; constructing this value authenticates nothing. No paths/credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct Binding {
    attempt: Uuid,
    intent_sha256: String,
    registered_identity_sha256: String,
}

impl Binding {
    pub fn new(
        attempt: Uuid,
        intent_sha256: &str,
        registered_identity_sha256: &str,
    ) -> Result<Self> {
        if attempt.is_nil() || !hex(intent_sha256, 64) || !hex(registered_identity_sha256, 64) {
            bail!("update_launcher_attempt_mismatch");
        }
        Ok(Self {
            attempt,
            intent_sha256: intent_sha256.to_owned(),
            registered_identity_sha256: registered_identity_sha256.to_owned(),
        })
    }
}

/// The single irreversible supervisor decision; uncertainty never reopens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Pending,
    Accepted,
    ClosedWithoutPermit,
    Uncertain,
}

/// Summary of the still-open, authenticated receiver's bounded drain to EOF.
/// A transport implementation must establish these facts, not infer them from
/// an elapsed timeout, successful write, missing ACK, PID or available lock.
#[cfg(test)]
enum EofObservation {
    EmptyAndAllWritersProvenGone,
    BufferedPermit,
    PartialFrame,
    ReadFailure,
    WriterOwnershipUnknown,
}

pub struct Arbitration {
    binding: Binding,
    decision: Decision,
}

impl Arbitration {
    pub fn new(binding: Binding) -> Self {
        Self {
            binding,
            decision: Decision::Pending,
        }
    }

    pub fn decision(&self) -> Decision {
        self.decision
    }

    // Used by the authenticated read boundary as well as the pure decision
    // model. Equality rejects stale metadata; it does not attest its origin.
    pub(super) fn match_binding(&mut self, observed: &Binding) -> Result<()> {
        if self.binding != *observed {
            if self.decision == Decision::Pending {
                self.decision = Decision::Uncertain;
            }
            bail!("update_launcher_attempt_mismatch");
        }
        Ok(())
    }

    /// Consume exactly one authenticated connection-bound frame. Caller-created
    /// Binding or raw framing alone cannot enter this boundary. This decision
    /// still proves neither durable registration nor owner receipt of an ACK.
    pub fn accept(&mut self, observed: super::handoff::LauncherFrame<'_>) -> Result<()> {
        let binding = observed.permit_binding(self)?;
        self.accept_binding(binding)
    }

    fn accept_binding(&mut self, observed: &Binding) -> Result<()> {
        self.match_binding(observed)?;
        if self.decision != Decision::Pending {
            bail!("update_recovery_required");
        }
        self.decision = Decision::Accepted;
        Ok(())
    }

    /// Select noexec only after authenticated empty EOF with all writers gone,
    /// while the receiver still exists. The caller must subsequently close that
    /// receiver and persist/read back a bound noexec record before any recovery
    /// classification. This decision alone proves no durable terminal outcome.
    #[cfg(test)]
    fn finish_eof(&mut self, observed: &Binding, observation: EofObservation) -> Result<()> {
        self.match_binding(observed)?;
        if self.decision != Decision::Pending {
            bail!("update_recovery_required");
        }
        if !matches!(observation, EofObservation::EmptyAndAllWritersProvenGone) {
            self.decision = Decision::Uncertain;
            bail!("update_recovery_required");
        }
        self.decision = Decision::ClosedWithoutPermit;
        Ok(())
    }

    /// Read/identity/transport ambiguity cannot be retried into acceptance or
    /// noexec. Already-terminal decisions remain irreversible, even on errors.
    pub fn invalidate(&mut self) {
        if self.decision == Decision::Pending {
            self.decision = Decision::Uncertain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(attempt: u128, intent: char, identity: char) -> Binding {
        Binding::new(
            Uuid::from_u128(attempt),
            &intent.to_string().repeat(64),
            &identity.to_string().repeat(64),
        )
        .unwrap()
    }

    #[test]
    fn serialized_permit_eof_and_error_orders_cannot_cross_terminal_branches_or_replay() {
        // Enumerate every length-4 interleaving, including repeats, of permit,
        // authenticated empty EOF, uncertainty, and buffered/partial EOF.
        for sequence in 0..625 {
            let expected = binding(1, 'a', 'b');
            let mut gate = Arbitration::new(expected.clone());
            let mut digits = sequence;
            let mut successes = 0;
            for _ in 0..4 {
                let before = gate.decision();
                let event = digits % 5;
                let result = match event {
                    0 => gate.accept_binding(&expected),
                    1 => gate.finish_eof(&expected, EofObservation::EmptyAndAllWritersProvenGone),
                    2 => {
                        gate.invalidate();
                        Err(anyhow::anyhow!("test uncertainty"))
                    }
                    3 => gate.finish_eof(&expected, EofObservation::BufferedPermit),
                    _ => gate.finish_eof(&expected, EofObservation::PartialFrame),
                };
                digits /= 5;
                successes += usize::from(result.is_ok());
                if before != Decision::Pending {
                    assert_eq!(gate.decision(), before);
                    assert!(result.is_err());
                } else {
                    let expected = match event {
                        0 => Decision::Accepted,
                        1 => Decision::ClosedWithoutPermit,
                        _ => Decision::Uncertain,
                    };
                    assert_eq!(gate.decision(), expected);
                    assert_eq!(result.is_ok(), event < 2);
                }
            }
            assert!(successes <= 1);
        }
    }

    #[test]
    fn stale_attempt_changed_intent_or_registered_identity_fail_closed_without_echoing_values() {
        let current = binding(2, 'a', 'b');
        for foreign in [
            binding(1, 'a', 'b'),
            binding(3, 'a', 'b'),
            binding(2, 'c', 'b'),
            binding(2, 'a', 'd'),
        ] {
            for eof in [false, true] {
                let mut gate = Arbitration::new(current.clone());
                let result = if eof {
                    gate.finish_eof(&foreign, EofObservation::EmptyAndAllWritersProvenGone)
                } else {
                    gate.accept_binding(&foreign)
                };
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "update_launcher_attempt_mismatch"
                );
                assert_eq!(gate.decision(), Decision::Uncertain);
                assert!(gate.accept_binding(&current).is_err());
                assert!(gate
                    .finish_eof(&current, EofObservation::EmptyAndAllWritersProvenGone)
                    .is_err());
            }
        }
    }

    #[test]
    fn eof_read_error_or_unknown_writers_cannot_create_noexec() {
        for observation in [
            EofObservation::ReadFailure,
            EofObservation::WriterOwnershipUnknown,
        ] {
            let expected = binding(1, 'a', 'b');
            let mut gate = Arbitration::new(expected.clone());
            assert_eq!(
                gate.finish_eof(&expected, observation)
                    .unwrap_err()
                    .to_string(),
                "update_recovery_required"
            );
            assert_eq!(gate.decision(), Decision::Uncertain);
        }
    }

    #[test]
    fn foreign_inputs_do_not_reopen_either_terminal_decision() {
        let expected = binding(1, 'a', 'b');
        let foreign = binding(2, 'a', 'b');
        for accept in [false, true] {
            let mut gate = Arbitration::new(expected.clone());
            if accept {
                gate.accept_binding(&expected).unwrap();
            } else {
                gate.finish_eof(&expected, EofObservation::EmptyAndAllWritersProvenGone)
                    .unwrap();
            }
            let decision = gate.decision();
            assert_eq!(
                gate.accept_binding(&foreign).unwrap_err().to_string(),
                "update_launcher_attempt_mismatch"
            );
            assert_eq!(
                gate.finish_eof(&foreign, EofObservation::EmptyAndAllWritersProvenGone)
                    .unwrap_err()
                    .to_string(),
                "update_launcher_attempt_mismatch"
            );
            gate.invalidate();
            assert_eq!(gate.decision(), decision);
        }
    }
}
