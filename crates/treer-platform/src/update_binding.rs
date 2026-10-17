//! Test-only inert frame model. Binding inputs must eventually come from an
//! authenticated durable record, not an untrusted peer. No permit or IO API.
use std::time::Instant;

const MAGIC_VERSION: &[u8; 5] = b"TRUB\x01";
pub(super) const FRAME_SIZE: usize = 118;
pub(super) type Frame = [u8; FRAME_SIZE];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Binding {
    pub attempt: [u8; 16],
    pub intent: [u8; 32],
    pub registration: [u8; 32],
}

#[derive(Clone, Copy)]
enum State {
    OwnerReady,
    OwnerReply,
    OwnerAck,
    ChildHello,
    ChildFinish,
    Complete,
    Refused,
}

pub(super) struct Exchange {
    binding: Binding,
    local: [u8; 16],
    owner: [u8; 16],
    child: [u8; 16],
    state: State,
    deadline: Instant,
}

impl Exchange {
    pub(super) fn new(binding: Binding, nonce: [u8; 16], owner: bool, deadline: Instant) -> Self {
        let valid = nonce != [0; 16]
            && binding.attempt != [0; 16]
            && binding.intent != [0; 32]
            && binding.registration != [0; 32];
        Self {
            binding,
            local: nonce,
            owner: [0; 16],
            child: [0; 16],
            deadline,
            state: if !valid {
                State::Refused
            } else if owner {
                State::OwnerReady
            } else {
                State::ChildHello
            },
        }
    }

    fn frame(&self, tag: u8) -> Frame {
        let mut bytes = [0; FRAME_SIZE];
        bytes[..5].copy_from_slice(MAGIC_VERSION);
        bytes[5] = tag;
        bytes[6..22].copy_from_slice(&self.binding.attempt);
        bytes[22..54].copy_from_slice(&self.binding.intent);
        bytes[54..86].copy_from_slice(&self.binding.registration);
        bytes[86..102].copy_from_slice(&self.owner);
        bytes[102..118].copy_from_slice(&self.child);
        bytes
    }

    pub(super) fn start(&mut self, now: Instant) -> Result<Frame, ()> {
        let prior = std::mem::replace(&mut self.state, State::Refused);
        if now >= self.deadline || !matches!(prior, State::OwnerReady) {
            return Err(());
        }
        self.owner = self.local;
        self.state = State::OwnerReply;
        Ok(self.frame(1))
    }

    /// Every failure poisons the session, including an extra frame after
    /// completion. Absolute time is supplied by the transport's monotonic clock.
    pub(super) fn receive(&mut self, bytes: &[u8], now: Instant) -> Result<Option<Frame>, ()> {
        let prior = std::mem::replace(&mut self.state, State::Refused);
        if now >= self.deadline
            || bytes.len() != FRAME_SIZE
            || &bytes[..5] != MAGIC_VERSION
            || bytes[6..22] != self.binding.attempt
            || bytes[22..54] != self.binding.intent
            || bytes[54..86] != self.binding.registration
        {
            return Err(());
        }
        let owner: [u8; 16] = bytes[86..102].try_into().map_err(|_| ())?;
        let child: [u8; 16] = bytes[102..118].try_into().map_err(|_| ())?;
        match (prior, bytes[5]) {
            (State::ChildHello, 1)
                if owner != [0; 16] && child == [0; 16] && owner != self.local =>
            {
                self.owner = owner;
                self.child = self.local;
                self.state = State::ChildFinish;
                Ok(Some(self.frame(2)))
            }
            (State::OwnerReply, 2) if owner == self.owner && child != [0; 16] && child != owner => {
                self.child = child;
                self.state = State::OwnerAck;
                Ok(Some(self.frame(3)))
            }
            (State::ChildFinish, 3) if owner == self.owner && child == self.child => {
                self.state = State::Complete;
                Ok(Some(self.frame(4)))
            }
            (State::OwnerAck, 4) if owner == self.owner && child == self.child => {
                self.state = State::Complete;
                Ok(None)
            }
            _ => Err(()),
        }
    }

    pub(super) fn complete(&self) -> bool {
        matches!(self.state, State::Complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn pair() -> (Exchange, Exchange, Instant) {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(5);
        let binding = Binding {
            attempt: [1; 16],
            intent: [2; 32],
            registration: [3; 32],
        };
        (
            Exchange::new(binding, [4; 16], true, deadline),
            Exchange::new(binding, [5; 16], false, deadline),
            now,
        )
    }

    #[test]
    fn completes_only_the_ordered_mutually_bound_exchange() {
        let (mut owner, mut child, now) = pair();
        let hello = owner.start(now).unwrap();
        let reply = child.receive(&hello, now).unwrap().unwrap();
        let finish = owner.receive(&reply, now).unwrap().unwrap();
        assert!(!owner.complete() && !child.complete());
        let ack = child.receive(&finish, now).unwrap().unwrap();
        assert!(child.complete());
        assert!(owner.receive(&ack, now).unwrap().is_none());
        assert!(owner.complete());
        assert!(owner.receive(&ack, now).is_err());
        assert!(!owner.complete());
    }

    #[test]
    fn every_modified_byte_or_wrong_length_poisoned_without_recovery() {
        for offset in 0..FRAME_SIZE {
            let (mut owner, mut child, now) = pair();
            let hello = owner.start(now).unwrap();
            let mut bad = hello;
            bad[offset] ^= 0x80;
            // A changed nonzero owner challenge is allowed in the first hello,
            // but must fail when echoed to the owner that selected the original.
            match child.receive(&bad, now) {
                Ok(Some(reply)) => {
                    assert!((86..102).contains(&offset));
                    assert!(owner.receive(&reply, now).is_err());
                    assert!(owner.receive(&reply, now).is_err());
                }
                Err(()) => assert!(child.receive(&hello, now).is_err()),
                _ => panic!("unexpected completion"),
            }
        }
        for size in [0, 1, FRAME_SIZE - 1, FRAME_SIZE + 1, 4096] {
            let (_, mut child, now) = pair();
            assert!(child.receive(&vec![0; size], now).is_err());
        }
    }

    #[test]
    fn rejects_duplicates_reordering_replay_and_deadline_extension() {
        let (mut owner, mut child, now) = pair();
        let hello = owner.start(now).unwrap();
        let reply = child.receive(&hello, now).unwrap().unwrap();
        assert!(child.receive(&hello, now).is_err());
        assert!(owner.receive(&hello, now).is_err());
        let (mut other, _, _) = pair();
        other.local = [6; 16];
        other.start(now).unwrap();
        assert!(other.receive(&reply, now).is_err());
        let (_, mut expired, now) = pair();
        assert!(expired
            .receive(&hello, now + Duration::from_secs(5))
            .is_err());
        assert!(expired.receive(&hello, now).is_err());
        let (mut owner, _, now) = pair();
        assert!(owner.start(now + Duration::from_secs(5)).is_err());
        assert!(owner.start(now).is_err());
    }

    #[test]
    fn refuses_empty_binding_inputs_and_wrong_format_before_exchange() {
        let (mut valid, _, now) = pair();
        let hello = valid.start(now).unwrap();
        for missing in 0..4 {
            let mut binding = valid.binding;
            let mut nonce = [5; 16];
            match missing {
                0 => binding.attempt = [0; 16],
                1 => binding.intent = [0; 32],
                2 => binding.registration = [0; 32],
                _ => nonce = [0; 16],
            }
            for owner in [false, true] {
                let mut invalid = Exchange::new(binding, nonce, owner, valid.deadline);
                if owner {
                    assert!(invalid.start(now).is_err());
                }
                assert!(invalid.receive(&hello, now).is_err());
            }
        }
        for header in [*b"NOPE\x01", *b"TRUB\x00", *b"TRUB\x02"] {
            let (_, mut child, _) = pair();
            let mut wrong = hello;
            wrong[..5].copy_from_slice(&header);
            assert!(child.receive(&wrong, now).is_err());
            assert!(child.receive(&hello, now).is_err());
        }
    }
}
