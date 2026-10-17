//! Inert fixture model only: callers supply same-boot GetTickCount64 readings.
//! No clock acquisition, serialized record, process, IO or authority API.

const MAX_BUDGET_MS: u64 = 5_000;

#[derive(Clone, Copy)]
struct Window {
    start: u64,
    expiry: u64,
}

struct Deadline {
    expiry: u64,
    last: u64,
    refused: bool,
}

impl Window {
    fn begin(now: u64) -> Result<Self, ()> {
        Ok(Self {
            start: now,
            expiry: now.checked_add(MAX_BUDGET_MS).ok_or(())?,
        })
    }

    // Both readers validate independently. The owner additionally supplies its
    // original local expiry; record corruption must not extend that deadline.
    fn accept(self, now: u64, original_expiry: Option<u64>) -> Result<Deadline, ()> {
        let span = self.expiry.checked_sub(self.start).ok_or(())?;
        if self.start > now || span == 0 || span > MAX_BUDGET_MS {
            return Err(());
        }
        let cap = now.checked_add(MAX_BUDGET_MS).ok_or(())?;
        let expiry = self.expiry.min(cap).min(original_expiry.unwrap_or(cap));
        if now >= expiry {
            return Err(());
        }
        Ok(Deadline {
            expiry,
            last: now,
            refused: false,
        })
    }
}

impl Deadline {
    // Invoke after each blocking phase/wake, independently of async timers.
    // An anomalous or expired sample permanently poisons this local attempt.
    fn check(&mut self, now: u64) -> Result<(), ()> {
        if self.refused || now < self.last || now >= self.expiry {
            self.refused = true;
            return Err(());
        }
        self.last = now;
        Ok(())
    }
}

#[test]
fn independent_readers_share_original_expiry_without_receive_budget() {
    let window = Window::begin(100).unwrap();
    let mut owner = window.accept(150, Some(window.expiry)).unwrap();
    let mut child = window.accept(4_900, None).unwrap();
    assert!(owner.check(5_099).is_ok());
    assert!(child.check(5_099).is_ok());
    assert!(owner.check(5_100).is_err());
    assert!(child.check(5_100).is_err());
    assert!(window.accept(5_100, None).is_err());
}

#[test]
fn corrupt_windows_and_clock_arithmetic_fail_closed() {
    assert!(Window::begin(u64::MAX - MAX_BUDGET_MS + 1).is_err());
    for (start, expiry, now) in [
        (100, 99, 100),
        (100, 100, 100),
        (100, 5_101, 100),
        (100, 200, 99),
        (100, 200, 200),
        (u64::MAX - 2, u64::MAX, u64::MAX - 1),
    ] {
        assert!(Window { start, expiry }.accept(now, None).is_err());
    }
    let original = Window::begin(100).unwrap();
    let changed = Window {
        start: 200,
        expiry: 5_200,
    };
    let mut owner = changed.accept(200, Some(original.expiry)).unwrap();
    assert_eq!(owner.expiry, original.expiry);
    assert!(owner.check(original.expiry).is_err());
}

#[test]
fn backward_or_suspend_sized_jump_permanently_refuses_attempt() {
    for bad in [199, 5_100, 100_000] {
        let mut deadline = Window::begin(100).unwrap().accept(200, None).unwrap();
        assert!(deadline.check(200).is_ok());
        assert!(deadline.check(bad).is_err());
        assert!(deadline.check(201).is_err());
    }
}

#[test]
fn child_with_later_window_cannot_complete_owner_after_reply_or_ack_expiry() {
    use super::update_binding::{Binding, Exchange};
    use std::time::{Duration, Instant};

    // Synthetic mapping only, not the native clock adapter: both local windows
    // feed their independently bounded deadlines into the existing frame model.
    for expire_at_ack in [false, true] {
        let original = Window::begin(100).unwrap();
        let corrupt = Window::begin(200).unwrap();
        let mut owner_window = corrupt.accept(200, Some(original.expiry)).unwrap();
        let mut child_window = corrupt.accept(200, None).unwrap();
        let epoch = Instant::now();
        let at = |ms| epoch + Duration::from_millis(ms);
        let binding = Binding {
            attempt: [1; 16],
            intent: [2; 32],
            registration: [3; 32],
        };
        let mut owner = Exchange::new(binding, [4; 16], true, at(owner_window.expiry));
        let mut child = Exchange::new(binding, [5; 16], false, at(child_window.expiry));
        let hello = owner.start(at(200)).unwrap();
        let reply = child.receive(&hello, at(200)).unwrap().unwrap();
        let late_frame = if expire_at_ack {
            let finish = owner.receive(&reply, at(200)).unwrap().unwrap();
            child.receive(&finish, at(5_100)).unwrap().unwrap()
        } else {
            reply
        };
        assert!(child_window.check(5_100).is_ok());
        assert!(owner_window.check(5_100).is_err());
        assert!(owner.receive(&late_frame, at(5_100)).is_err());
        assert!(!owner.complete());
        // Even replay with an earlier sample cannot revive owner completion.
        assert!(owner.receive(&late_frame, at(201)).is_err());
        assert!(!(owner.complete() && child.complete()));
    }
    // Exchange completion is diagnostic only; these models expose no permit or
    // authority constructor, including when the child alone has reached ACK.
}
