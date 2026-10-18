//! Per-instance admission for chunked machine uploads (#166): a bounded
//! number of part bodies in flight on this Proxy (memory), and fixed-window
//! rate limits per key (parts per upload, status polls per upload). Quotas
//! that must hold across instances live in the database (`upload_store`).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Part bodies this instance streams at once.
pub const PART_BODIES_PER_INSTANCE: usize = 32;
pub const PARTS_PER_UPLOAD_PER_MINUTE: usize = 160;
pub const STATUS_PER_UPLOAD_PER_MINUTE: usize = 60;
pub const RETRY_AFTER_SECONDS: u64 = 5;
const WINDOW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct GateInner {
    in_flight: usize,
    windows: HashMap<String, (Instant, usize)>,
}

#[derive(Default)]
pub struct UploadGate {
    inner: Mutex<GateInner>,
}

/// One streaming part body on this instance; dropping it frees the slot.
pub struct BodyPermit {
    gate: Arc<UploadGate>,
}

impl std::fmt::Debug for BodyPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BodyPermit")
    }
}

impl UploadGate {
    pub fn acquire_body(self: &Arc<Self>) -> Option<BodyPermit> {
        let mut inner = self.lock();
        if inner.in_flight >= PART_BODIES_PER_INSTANCE {
            return None;
        }
        inner.in_flight += 1;
        Some(BodyPermit {
            gate: Arc::clone(self),
        })
    }

    /// Counts one event for `key` in the current fixed minute; `false` when
    /// the limit is reached. The table is swept of expired windows on use.
    pub fn allow(&self, key: &str, limit: usize) -> bool {
        let mut inner = self.lock();
        let now = Instant::now();
        if inner.windows.len() > 4096 {
            inner
                .windows
                .retain(|_, (started, _)| now.duration_since(*started) < WINDOW);
        }
        let entry = inner.windows.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= limit {
            return false;
        }
        entry.1 += 1;
        true
    }

    #[cfg(test)]
    pub fn in_flight(&self) -> usize {
        self.lock().in_flight
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for BodyPermit {
    fn drop(&mut self) {
        let mut inner = self.gate.lock();
        inner.in_flight = inner.in_flight.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_slots_are_bounded_and_freed_on_drop() {
        let gate = Arc::new(UploadGate::default());
        let permits: Vec<_> = (0..PART_BODIES_PER_INSTANCE)
            .map(|_| gate.acquire_body().unwrap())
            .collect();
        assert!(gate.acquire_body().is_none());
        drop(permits);
        assert_eq!(gate.in_flight(), 0);
        assert!(gate.acquire_body().is_some());
    }

    #[test]
    fn rate_windows_are_per_key() {
        let gate = UploadGate::default();
        for _ in 0..3 {
            assert!(gate.allow("a", 3));
        }
        assert!(!gate.allow("a", 3));
        assert!(gate.allow("b", 3));
    }
}
