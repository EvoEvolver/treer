//! Private hosted fixture clock adapter. No installed state or authority API.
use crate::update_deadline::{Deadline, Window};
use std::{io, time::Duration};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;

fn now() -> u64 {
    // The Windows contract includes sleep/hibernation; native tests below only
    // exercise ordinary elapsed time, not actual system power transitions.
    unsafe { GetTickCount64() }
}

fn refused(_: ()) -> io::Error {
    super::denied()
}

pub(super) struct TickDeadline {
    window: Window,
    deadline: Deadline,
}

impl TickDeadline {
    pub(super) fn begin() -> io::Result<Self> {
        let sampled = now();
        let window = Window::begin(sampled).map_err(refused)?;
        let deadline = window
            .accept(sampled, Some(window.expiry))
            .map_err(refused)?;
        Ok(Self { window, deadline })
    }

    pub(super) fn accept(window: Window, owner_original: Option<u64>) -> io::Result<Self> {
        let deadline = window.accept(now(), owner_original).map_err(refused)?;
        Ok(Self { window, deadline })
    }

    pub(super) fn remaining(&mut self) -> io::Result<Duration> {
        self.deadline
            .remaining_ms(now())
            .map(Duration::from_millis)
            .map_err(refused)
    }

    // Tokio bounds waiting, but its timer is not evidence that Windows' clock
    // stayed in range across a blocking operation or power-state transition.
    pub(super) async fn run<T>(
        &mut self,
        future: impl std::future::Future<Output = io::Result<T>>,
    ) -> io::Result<T> {
        let budget = self.remaining()?;
        let result = tokio::time::timeout(budget, future).await;
        self.remaining()?;
        match result {
            Ok(Ok(value)) => Ok(value),
            _ => {
                self.deadline.refuse();
                Err(super::denied())
            }
        }
    }
}

#[test]
fn native_tick_windows_keep_owner_ceiling_and_expire() {
    let mut owner = TickDeadline::begin().unwrap();
    let mut child = TickDeadline::accept(owner.window, None).unwrap();
    assert!(owner.remaining().unwrap() <= Duration::from_secs(5));
    assert!(child.remaining().unwrap() <= Duration::from_secs(5));

    // A short disposable window exercises real clock expiry without waiting
    // five seconds. This sleeps a thread, not Windows or a live machine.
    let sampled = now();
    let short = Window {
        start: sampled,
        expiry: sampled.checked_add(25).unwrap(),
    };
    let short_owner = TickDeadline::accept(short, Some(short.expiry));
    std::thread::sleep(Duration::from_millis(60));
    assert!(now() >= short.expiry);
    if let Ok(mut short_owner) = short_owner {
        assert!(short_owner.remaining().is_err());
        assert!(short_owner.remaining().is_err());
    }
    assert!(TickDeadline::accept(short, None).is_err());

    let sampled = now();
    let future = Window {
        start: sampled.checked_add(60_000).unwrap(),
        expiry: sampled.checked_add(60_001).unwrap(),
    };
    assert!(TickDeadline::accept(future, None).is_err());
    // Even a valid record cannot revive the original owner's expired ceiling.
    let valid = Window::begin(sampled).unwrap();
    assert!(TickDeadline::accept(valid, Some(short.expiry)).is_err());
}
