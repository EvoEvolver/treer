//! Actual bounded Core workers. Network calls never hold the SQLite state lock.
//! These workers are embedded by the future hub executable, not a third program.
use crate::{
    core::{Core, ReceiveBatch},
    hub::{Directory, Hub, Readback},
    relay::Mailbox,
};
use serde::Serialize;
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub type SharedHub<M> = Arc<Mutex<Hub<M>>>;
pub trait CoreInbox: Mailbox + Directory + Clone + Send + 'static {
    fn receive_batch(&self, wait_ms: u32) -> Result<ReceiveBatch, crate::core::Error>;
}
impl CoreInbox for Core {
    fn receive_batch(&self, wait_ms: u32) -> Result<ReceiveBatch, crate::core::Error> {
        Core::receive_batch(self, wait_ms)
    }
}
const MAX_PAGES: usize = 10;
const DRAIN_BUDGET: Duration = Duration::from_millis(250);
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Counters {
    #[cfg(test)]
    refresh_gate: Option<Arc<AtomicBool>>,
    pages: AtomicU64,
    acked: AtomicU64,
    failures: AtomicU64,
    maintenance: AtomicU64,
    readbacks: AtomicU64,
    stopping: AtomicBool,
    consecutive_failures: AtomicU64,
    last_inbox_failure: AtomicU64,
    oldest: Mutex<Option<(String, Instant)>>,
}
fn increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_add(1))
    });
}
#[derive(Default, Serialize)]
pub struct WorkerStatus {
    pub inbound_core_handoffs: u64,
    pub notice_core_handoffs: u64,
    pub handoff_failures: u64,
    pub handoffs_stopped: bool,
    pub cli_integrity_failed: bool,
    pub last_handoff_failure: Option<&'static str>,
    pub receive_pages: u64,
    pub core_acked: u64,
    pub failures: u64,
    pub maintenance_ticks: u64,
    pub readback_completions: u64,
    pub stopping: bool,
    pub consecutive_failed_drains: u64,
    pub last_inbox_failure: Option<&'static str>,
    /// Local monotonic observation age, NOT Core message creation age.
    pub oldest_unacked_observed_age_seconds: Option<u64>,
}
/// Owns only threads started by this object. No process names, external signals
/// or restarts. Stop waits for bounded Core CLI calls already in flight.
pub struct CoreWorkers {
    shared: Arc<Counters>,
    joins: Vec<JoinHandle<()>>,
}
impl CoreWorkers {
    pub fn start<M: CoreInbox>(state: SharedHub<M>) -> std::io::Result<Self> {
        Self::start_with_counters(state, Arc::new(Counters::default()))
    }
    #[cfg(all(test, feature = "hub"))]
    pub(crate) fn with_test_readback_gate_159<M: CoreInbox>(
        state: SharedHub<M>,
        gate: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        Self::start_with_counters(
            state,
            Arc::new(Counters {
                refresh_gate: Some(gate),
                ..Counters::default()
            }),
        )
    }
    fn start_with_counters<M: CoreInbox>(
        state: SharedHub<M>,
        shared: Arc<Counters>,
    ) -> std::io::Result<Self> {
        let backend = state
            .lock()
            .map_err(|_| std::io::Error::other("hub_state_unavailable"))?
            .backend()
            .clone();
        let mut workers = Self {
            shared,
            joins: vec![],
        };
        let result: std::io::Result<()> =
            (|| {
                let s = state.clone();
                let c = workers.shared.clone();
                workers
                    .joins
                    .push(thread::Builder::new().name("hub-core-inbox".into()).spawn(
                        move || {
                            while !c.stopping.load(Ordering::Acquire) {
                                let good = drain(&s, &backend, &c, MAX_PAGES, DRAIN_BUDGET);
                                // Yield on healthy backlog; exponential backoff
                                // prevents persistent unacked poison/storage
                                // faults hammering Core and the local disk.
                                wait(&c, record_inbox_result(&c, good));
                            }
                        },
                    )?);
                let s = state.clone();
                let c = workers.shared.clone();
                workers.joins.push(
                    thread::Builder::new()
                        .name("hub-maintenance".into())
                        .spawn(move || {
                            while !c.stopping.load(Ordering::Acquire) {
                                if maintenance(&s, unix_now()).is_ok() {
                                    increment(&c.maintenance);
                                } else {
                                    increment(&c.failures);
                                }
                                #[cfg(feature = "hub")]
                                if crate::management::dispatch_one(&s, unix_now()).is_err() {
                                    increment(&c.failures);
                                }
                                wait(&c, MAINTENANCE_INTERVAL);
                            }
                        })?,
                );
                let c = workers.shared.clone();
                workers.joins.push(
                    thread::Builder::new()
                        .name("hub-readback-supervisor".into())
                        .spawn(move || supervise(state, c))?,
                );
                Ok(())
            })();
        // On spawn failure Drop stops and joins only the threads we created.
        result?;
        Ok(workers)
    }
    pub fn status(&self) -> WorkerStatus {
        let c = &self.shared;
        WorkerStatus {
            inbound_core_handoffs: 0,
            notice_core_handoffs: 0,
            handoff_failures: 0,
            handoffs_stopped: false,
            cli_integrity_failed: false,
            last_handoff_failure: None,
            receive_pages: c.pages.load(Ordering::Relaxed),
            core_acked: c.acked.load(Ordering::Relaxed),
            failures: c.failures.load(Ordering::Relaxed),
            maintenance_ticks: c.maintenance.load(Ordering::Relaxed),
            readback_completions: c.readbacks.load(Ordering::Relaxed),
            stopping: c.stopping.load(Ordering::Acquire),
            consecutive_failed_drains: c.consecutive_failures.load(Ordering::Relaxed),
            last_inbox_failure: failure_code(c.last_inbox_failure.load(Ordering::Relaxed)),
            oldest_unacked_observed_age_seconds: c
                .oldest
                .lock()
                .ok()
                .and_then(|old| old.as_ref().map(|(_, at)| at.elapsed().as_secs())),
        }
    }
    pub fn request_stop(&self) {
        self.shared.stopping.store(true, Ordering::Release);
    }
    pub fn stop(&mut self) {
        self.request_stop();
        for join in self.joins.drain(..) {
            if join.join().is_err() {
                increment(&self.shared.failures);
            }
        }
    }
}
impl Drop for CoreWorkers {
    fn drop(&mut self) {
        self.stop();
    }
}
fn failure_code(code: u64) -> Option<&'static str> {
    match code {
        1 => Some("core_receive"),
        2 => Some("core_ack"),
        3 => Some("storage"),
        4 => Some("binding"),
        5 => Some("failure_budget"),
        6 => Some("state_unavailable"),
        7 => Some("invalid_batch"),
        8 => Some("revision"),
        9 => Some("refetch_rate_limited"),
        10 => Some("cli_integrity_failed"),
        _ => None,
    }
}
fn hub_failure(error: crate::hub::Error) -> u64 {
    match error {
        crate::hub::Error::Integrity | crate::hub::Error::Core(crate::core::Error::Integrity) => 10,
        crate::hub::Error::Storage(_) => 3,
        crate::hub::Error::Core(_) => 1,
        crate::hub::Error::Binding => 4,
        crate::hub::Error::FailureBudget => 5,
        crate::hub::Error::Revision => 8,
        crate::hub::Error::RefetchRateLimited => 9,
    }
}
fn record_inbox_result(c: &Counters, good: bool) -> Duration {
    if good {
        c.consecutive_failures.store(0, Ordering::Relaxed);
        c.last_inbox_failure.store(0, Ordering::Relaxed);
        return Duration::from_millis(100);
    }
    increment(&c.failures);
    increment(&c.consecutive_failures);
    let streak = c.consecutive_failures.load(Ordering::Relaxed);
    Duration::from_secs(
        1u64.checked_shl(streak.saturating_sub(1).min(5) as u32)
            .unwrap_or(30)
            .min(30),
    )
}
fn observe(c: &Counters, id: Option<&str>, new_page: bool) {
    if let Ok(mut oldest) = c.oldest.lock() {
        match id {
            None => *oldest = None,
            Some(id)
                if oldest.is_none()
                    || (new_page && oldest.as_ref().is_some_and(|(old, _)| old != id)) =>
            {
                *oldest = Some((id.into(), Instant::now()))
            }
            _ => {}
        }
    }
}
fn observed_ack(c: &Counters, id: &str) {
    if let Ok(mut oldest) = c.oldest.lock() {
        if oldest.as_ref().is_some_and(|(old, _)| old == id) {
            *oldest = None;
        }
    }
}
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}
fn wait(c: &Counters, duration: Duration) {
    let started = Instant::now();
    while !c.stopping.load(Ordering::Acquire) && started.elapsed() < duration {
        thread::sleep((duration - started.elapsed().min(duration)).min(Duration::from_millis(25)));
    }
}
/// A blocked network receive/ack cannot hold the state mutex. Page/time limits
/// bound a drain, not an in-flight CLI call (the adapter has its own 15s limit).
fn drain<M: CoreInbox>(
    state: &SharedHub<M>,
    backend: &M,
    c: &Counters,
    max_pages: usize,
    budget: Duration,
) -> bool {
    let started = Instant::now();
    let mut good = true;
    for page in 0..max_pages {
        if c.stopping.load(Ordering::Acquire) {
            break;
        }
        let batch = match backend.receive_batch(if page == 0 { 1000 } else { 0 }) {
            Ok(b) => b,
            Err(_) => {
                c.last_inbox_failure.store(1, Ordering::Relaxed);
                return false;
            }
        };
        increment(&c.pages);
        let empty = batch.deliveries.is_empty();
        // The production adapter caps this at ten. Defend against faulty test
        // or future adapters rather than doing unbounded state work here.
        if batch.deliveries.len() > 10 {
            c.last_inbox_failure.store(7, Ordering::Relaxed);
            return false;
        }
        observe(
            c,
            batch.deliveries.first().map(|d| d.delivery_id.as_str()),
            true,
        );
        for delivery in batch.deliveries {
            if c.stopping.load(Ordering::Acquire) {
                return good;
            }
            observe(c, Some(&delivery.delivery_id), false);
            let admitted = match state.lock() {
                Ok(mut hub) => hub
                    .admit_core_delivery(&delivery, unix_now())
                    .map_err(hub_failure),
                Err(_) => Err(6),
            };
            match admitted {
                Ok(()) => match backend.ack(&delivery.delivery_id) {
                    Ok(()) => {
                        increment(&c.acked);
                        observed_ack(c, &delivery.delivery_id);
                    }
                    Err(_) => {
                        good = false;
                        c.last_inbox_failure.store(2, Ordering::Relaxed);
                    }
                },
                Err(code) => {
                    good = false;
                    c.last_inbox_failure.store(code, Ordering::Relaxed);
                }
            }
            // Always process at least one delivery, even if the initial long
            // poll used the budget. Do not begin another potentially 15s ack
            // after the elapsed-time budget. Unprocessed rows stay in Core.
            if started.elapsed() >= budget {
                return good;
            }
        }
        if empty || batch.remaining_unacknowledged == 0 || started.elapsed() >= budget {
            break;
        }
        // Newer healthy work is reached only by acking the oldest window;
        // there is no invented cursor/skip/nack. Release CPU/state every page.
        thread::yield_now();
    }
    good
}
fn maintenance<M: CoreInbox>(state: &SharedHub<M>, now: i64) -> Result<(), ()> {
    let mut hub = state.lock().map_err(|_| ())?;
    hub.maintenance(now).map_err(|_| ())?;
    for id in hub.binding_ids().map_err(|_| ())? {
        // An intentionally paused/stale binding is normal, not a worker error.
        match hub.promote_held(&id, now) {
            Ok(_) | Err(crate::hub::Error::Binding) => {}
            Err(_) => return Err(()),
        }
    }
    Ok(())
}
fn refresh<M: CoreInbox>(state: &SharedHub<M>, backend: &M, id: &str) -> bool {
    let (revision, requested_at) = match state.lock() {
        Ok(hub) => match hub.view(id) {
            Ok(v) => (v.revision, unix_now()),
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    // Missing/denied/failed metadata pauses through the ordinary sticky path.
    let observed = backend.metadata(id).ok();
    match state.lock() {
        Ok(mut hub) => hub
            .record_readback(
                id,
                revision,
                Readback {
                    observed,
                    requested_at,
                },
                unix_now(),
            )
            .is_ok(),
        Err(_) => false,
    }
}
fn supervise<M: CoreInbox>(state: SharedHub<M>, c: Arc<Counters>) {
    let mut started = HashSet::new();
    let mut threads = vec![];
    while !c.stopping.load(Ordering::Acquire) {
        let snapshot = match state.lock() {
            Ok(hub) => hub.binding_ids().map(|ids| (ids, hub.backend().clone())),
            Err(_) => {
                // A poisoned actor cannot be recovered by these workers. Stop
                // all owned loops before joining the per-binding children.
                increment(&c.failures);
                c.stopping.store(true, Ordering::Release);
                break;
            }
        };
        if let Ok((ids, backend)) = snapshot {
            for id in ids {
                if started.contains(&id) {
                    continue;
                }
                // At most twenty configured bindings; one slow Core metadata
                // query cannot starve refresh for another endpoint.
                let s = state.clone();
                let shared = c.clone();
                let core = backend.clone();
                let endpoint = id.clone();
                match thread::Builder::new()
                    .name("hub-endpoint-readback".into())
                    .spawn(move || {
                        while !shared.stopping.load(Ordering::Acquire) {
                            // Test-only deterministic scheduling; production
                            // has neither this field nor a configuration switch.
                            #[cfg(test)]
                            if shared
                                .refresh_gate
                                .as_ref()
                                .is_some_and(|g| !g.load(Ordering::Acquire))
                            {
                                wait(&shared, Duration::from_millis(25));
                                continue;
                            }
                            let began = Instant::now();
                            if refresh(&s, &core, &endpoint) {
                                increment(&shared.readbacks);
                            } else {
                                increment(&shared.failures);
                            }
                            wait(&shared, REFRESH_INTERVAL.saturating_sub(began.elapsed()));
                        }
                    }) {
                    Ok(join) => {
                        started.insert(id);
                        threads.push(join);
                    }
                    Err(_) => increment(&c.failures),
                }
            }
        } else {
            increment(&c.failures);
        }
        wait(&c, Duration::from_millis(250));
    }
    for join in threads {
        if join.join().is_err() {
            increment(&c.failures);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{AgentMetadata, Delivery},
        hub::Binding,
        protocol::{Envelope, Sender, SenderKind},
        recovery::tests::metadata,
        store::{Limits, Store},
    };
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    #[derive(Clone, Default)]
    struct Fake {
        queue: Arc<Mutex<VecDeque<Value>>>,
        acks: Arc<AtomicU64>,
        waits: Arc<Mutex<Vec<u32>>>,
        block_receive: Arc<AtomicBool>,
        receive_entered: Arc<AtomicBool>,
        block_metadata: Arc<AtomicBool>,
        metadata_entered: Arc<AtomicBool>,
        fail_ack: Arc<AtomicBool>,
        fail_receive: Arc<AtomicBool>,
    }
    impl Fake {
        fn stall(block: &AtomicBool, entered: &AtomicBool) {
            entered.store(true, Ordering::Release);
            let at = Instant::now();
            while block.load(Ordering::Acquire) && at.elapsed() < Duration::from_secs(3) {
                thread::sleep(Duration::from_millis(5));
            }
        }
        fn push(&self, id: &str, n: usize) {
            let body = Envelope::Outbound {
                delivery_id: format!("del_source_{n}"),
                message_id: format!("msg_source_{n}"),
                original_sender: Sender {
                    kind: SenderKind::Agent,
                    id: "ag_recipient".into(),
                },
                body: "synthetic body".into(),
                context_ids: vec![],
            }
            .fragments(id, 1)
            .unwrap()
            .remove(0);
            self.queue.lock().unwrap().push_back(json!({"delivery_id":format!("del_{n}"),"recipient":{"kind":"agent","id":"app_hub"},"message":{"message_id":format!("msg_{n}"),"sender":{"kind":"agent","id":id},"body":body,"context_ids":[]}}));
        }
    }
    impl Mailbox for Fake {
        fn receive(&self, _: u32) -> Result<Vec<Delivery>, crate::core::Error> {
            panic!("use backlog-aware receive")
        }
        fn ack(&self, id: &str) -> Result<(), crate::core::Error> {
            if self.fail_ack.load(Ordering::Relaxed) {
                return Err(crate::core::Error::Rejected);
            }
            let mut q = self.queue.lock().unwrap();
            let i = q
                .iter()
                .position(|m| m["delivery_id"] == id)
                .ok_or(crate::core::Error::Invalid)?;
            q.remove(i);
            increment(&self.acks);
            Ok(())
        }
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<Value>,
        ) -> Result<String, crate::core::Error> {
            panic!("inbox workers must not send")
        }
    }
    impl Directory for Fake {
        fn metadata(&self, id: &str) -> Result<AgentMetadata, crate::core::Error> {
            if id == "ag_slow" {
                Self::stall(&self.block_metadata, &self.metadata_entered);
            }
            let mut m = metadata();
            m.agent_id = id.into();
            m.path = Some(format!("/team/{id}"));
            Ok(m)
        }
    }
    impl CoreInbox for Fake {
        fn receive_batch(&self, wait: u32) -> Result<ReceiveBatch, crate::core::Error> {
            if self.fail_receive.load(Ordering::Relaxed) {
                return Err(crate::core::Error::Unavailable);
            }
            self.waits.lock().unwrap().push(wait);
            Self::stall(&self.block_receive, &self.receive_entered);
            let q = self.queue.lock().unwrap();
            Ok(ReceiveBatch {
                deliveries: q
                    .iter()
                    .take(10)
                    .map(|v| serde_json::from_value(v.clone()).unwrap())
                    .collect(),
                remaining_unacknowledged: q.len().saturating_sub(10) as u64,
            })
        }
    }
    fn fixture() -> (SharedHub<Fake>, Fake) {
        let fake = Fake::default();
        let mut hub = Hub::new(
            Store::memory(Limits::default()).unwrap(),
            fake.clone(),
            "app_hub",
            "ws_test",
        )
        .unwrap();
        for id in ["ag_slow", "ag_healthy"] {
            hub.add_binding(Binding {
                endpoint_id: id.into(),
                generation: 1,
                recipient: "ag_recipient".into(),
                expected_path: format!("/team/{id}"),
                expected_cwd: "/workspace/endpoint".into(),
                expected_server_id: "machine_test".into(),
            })
            .unwrap();
        }
        let state = Arc::new(Mutex::new(hub));
        (state, fake)
    }
    fn activate(state: &SharedHub<Fake>, id: &str) {
        let mut h = state.lock().unwrap();
        let rev = h.view(id).unwrap().revision;
        let m = h.backend().metadata(id).unwrap();
        let now = unix_now();
        h.resume_after_readback(
            id,
            rev,
            Readback {
                observed: Some(m),
                requested_at: now,
            },
            now,
        )
        .unwrap();
    }
    fn wait_until(mut f: impl FnMut() -> bool) {
        let at = Instant::now();
        while !f() {
            assert!(
                at.elapsed() < Duration::from_secs(2),
                "worker condition deadline"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn production_drain_reaches_healthy_after_paused_flood_and_yields_between_bounded_pages() {
        let (state, fake) = fixture();
        activate(&state, "ag_healthy");
        for n in 0..1100 {
            fake.push("ag_slow", n);
        }
        fake.push("ag_healthy", 1100);
        let c = Counters::default();
        let mut ticks = 0;
        while !fake.queue.lock().unwrap().is_empty() {
            let pages = c.pages.load(Ordering::Relaxed);
            assert!(drain(&state, &fake, &c, 10, Duration::from_secs(1)));
            assert!(c.pages.load(Ordering::Relaxed) - pages <= 10);
            maintenance(&state, unix_now()).unwrap();
            ticks += 1;
            assert!(ticks < 30);
        }
        assert!(ticks >= 12);
        assert_eq!(fake.acks.load(Ordering::Relaxed), 1101);
        let h = state.lock().unwrap();
        assert_eq!(h.ready_outbound("ag_healthy", unix_now()).unwrap().len(), 1);
        assert_eq!(h.hold_summary("ag_slow").unwrap().capacity_rejections, 1080);
        assert_eq!(h.hold_summary("ag_slow").unwrap().receipts, 1000);
        let faults: i64 = h
            .store
            .db
            .query_row("SELECT COUNT(*) FROM failures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(faults, 0);
        let waits = fake.waits.lock().unwrap();
        assert!(waits.contains(&0));
        assert!(waits.contains(&1000));
    }
    #[test]
    fn stalled_receive_does_not_block_state_maintenance_or_readback_workers() {
        let (state, fake) = fixture();
        fake.block_receive.store(true, Ordering::Release);
        let mut workers = CoreWorkers::start(state.clone()).unwrap();
        wait_until(|| fake.receive_entered.load(Ordering::Acquire));
        wait_until(|| {
            workers.status().maintenance_ticks >= 1 && workers.status().readback_completions >= 2
        });
        wait_until(|| state.try_lock().is_ok());
        {
            let h = state.lock().unwrap();
            assert!(h.view("ag_healthy").unwrap().checked_at.is_some());
            assert!(
                h.view("ag_healthy").unwrap().paused,
                "matching readback never resumes owner pause"
            );
        }
        fake.block_receive.store(false, Ordering::Release);
        workers.stop();
        assert!(workers.status().stopping);
        let pages = workers.status().receive_pages;
        thread::sleep(Duration::from_millis(30));
        assert_eq!(workers.status().receive_pages, pages);
    }
    #[test]
    fn slow_endpoint_metadata_does_not_starve_other_binding_refresh_or_ui_state() {
        let (state, fake) = fixture();
        fake.block_metadata.store(true, Ordering::Release);
        let mut workers = CoreWorkers::start(state.clone()).unwrap();
        wait_until(|| fake.metadata_entered.load(Ordering::Acquire));
        wait_until(|| {
            state
                .try_lock()
                .is_ok_and(|hub| hub.view("ag_healthy").unwrap().checked_at.is_some())
        });
        wait_until(|| state.try_lock().is_ok());
        assert!(state
            .lock()
            .unwrap()
            .view("ag_slow")
            .unwrap()
            .checked_at
            .is_none());
        fake.block_metadata.store(false, Ordering::Release);
        workers.stop();
    }
    #[test]
    fn storage_failure_never_acks_and_zero_budget_processes_only_one_delivery() {
        let (state, fake) = fixture();
        fake.push("ag_slow", 0);
        fake.push("ag_slow", 1);
        let c = Counters::default();
        assert!(drain(&state, &fake, &c, 10, Duration::ZERO));
        assert_eq!(fake.acks.load(Ordering::Relaxed), 1);
        assert_eq!(fake.queue.lock().unwrap().len(), 1);
        state
            .lock()
            .unwrap()
            .store
            .db
            .execute("UPDATE hub_bindings SET checked_at='damaged'", [])
            .unwrap();
        assert!(!drain(&state, &fake, &c, 1, Duration::from_secs(1)));
        assert_eq!(fake.acks.load(Ordering::Relaxed), 1);
        assert_eq!(fake.queue.lock().unwrap().len(), 1);
    }
    #[test]
    fn added_binding_gets_an_independent_readback_worker() {
        let (state, _fake) = fixture();
        let mut workers = CoreWorkers::start(state.clone()).unwrap();
        wait_until(|| workers.status().readback_completions >= 2);
        state
            .lock()
            .unwrap()
            .add_binding(Binding {
                endpoint_id: "ag_added".into(),
                generation: 1,
                recipient: "ag_recipient".into(),
                expected_path: "/team/ag_added".into(),
                expected_cwd: "/workspace/endpoint".into(),
                expected_server_id: "machine_test".into(),
            })
            .unwrap();
        wait_until(|| {
            state
                .try_lock()
                .is_ok_and(|h| h.view("ag_added").unwrap().checked_at.is_some())
        });
        workers.stop();
        assert!(workers.status().readback_completions >= 3);
    }
    #[test]
    fn poisoned_state_stops_all_owned_workers_without_join_deadlock() {
        let (state, _fake) = fixture();
        let mut workers = CoreWorkers::start(state.clone()).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = state.lock().unwrap();
            panic!("synthetic actor panic");
        }));
        assert!(result.is_err());
        wait_until(|| workers.status().stopping);
        workers.stop();
        assert!(workers.status().failures >= 1);
    }
    #[test]
    fn failed_inbox_drains_back_off_to_thirty_seconds_and_recovery_resets_status() {
        let workers = CoreWorkers {
            shared: Arc::new(Counters::default()),
            joins: vec![],
        };
        let c = &workers.shared;
        c.last_inbox_failure.store(3, Ordering::Relaxed);
        for seconds in [1, 2, 4, 8, 16, 30, 30] {
            assert_eq!(record_inbox_result(c, false), Duration::from_secs(seconds));
        }
        assert_eq!(workers.status().consecutive_failed_drains, 7);
        assert_eq!(workers.status().last_inbox_failure, Some("storage"));
        assert_eq!(record_inbox_result(c, true), Duration::from_millis(100));
        assert_eq!(workers.status().consecutive_failed_drains, 0);
        assert_eq!(workers.status().last_inbox_failure, None);
    }
    #[test]
    fn failed_ack_exposes_local_unacked_age_and_fixed_reason_until_redelivery_commits() {
        let (state, fake) = fixture();
        fake.push("ag_slow", 0);
        fake.fail_ack.store(true, Ordering::Relaxed);
        let workers = CoreWorkers {
            shared: Arc::new(Counters::default()),
            joins: vec![],
        };
        let c = &workers.shared;
        assert!(!drain(&state, &fake, c, 1, Duration::from_secs(1)));
        assert_eq!(workers.status().last_inbox_failure, Some("core_ack"));
        assert!(workers
            .status()
            .oldest_unacked_observed_age_seconds
            .is_some());
        // The public age is monotonic/local, and never includes the stored ID.
        c.oldest.lock().unwrap().as_mut().unwrap().1 = Instant::now() - Duration::from_secs(2);
        let status = serde_json::to_string(&workers.status()).unwrap();
        assert!(!status.contains("del_0"));
        assert!(
            workers
                .status()
                .oldest_unacked_observed_age_seconds
                .unwrap()
                >= 2
        );
        fake.fail_ack.store(false, Ordering::Relaxed);
        assert!(drain(&state, &fake, c, 1, Duration::from_secs(1)));
        assert_eq!(workers.status().oldest_unacked_observed_age_seconds, None);
        assert_eq!(fake.acks.load(Ordering::Relaxed), 1);
        assert_eq!(
            state
                .lock()
                .unwrap()
                .hold_summary("ag_slow")
                .unwrap()
                .held_fragments,
            1
        );
        fake.fail_receive.store(true, Ordering::Relaxed);
        assert!(!drain(&state, &fake, c, 1, Duration::from_secs(1)));
        assert_eq!(workers.status().last_inbox_failure, Some("core_receive"));
    }
    #[test]
    fn actual_worker_reports_storage_stall_then_resumes_without_restart() {
        let (state, fake) = fixture();
        fake.push("ag_slow", 0);
        state
            .lock()
            .unwrap()
            .store
            .db
            .execute("UPDATE hub_bindings SET checked_at='damaged'", [])
            .unwrap();
        let mut workers = CoreWorkers::start(state.clone()).unwrap();
        wait_until(|| workers.status().consecutive_failed_drains >= 1);
        assert_eq!(workers.status().last_inbox_failure, Some("storage"));
        assert_eq!(fake.acks.load(Ordering::Relaxed), 0);
        state
            .lock()
            .unwrap()
            .store
            .db
            .execute("UPDATE hub_bindings SET checked_at=NULL", [])
            .unwrap();
        wait_until(|| {
            fake.acks.load(Ordering::Relaxed) == 1
                && workers.status().consecutive_failed_drains == 0
        });
        workers.stop();
    }
}
