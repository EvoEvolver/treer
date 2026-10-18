//! Owned Core handoff workers. Each endpoint has its own bounded CLI sequence;
//! network waits never own the state lock or another endpoint's turn.
use crate::{
    hub::{Directory, Error as HubError},
    notices::NoticeIntent,
    relay::Mailbox,
    scheduler::SharedHub,
    telegram::CoreIntent,
};
use serde::Serialize;
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Default)]
struct Counters {
    stopping: AtomicBool,
    inbound: AtomicU64,
    notices: AtomicU64,
    failures: AtomicU64,
}
#[derive(Default, Serialize)]
pub struct Status {
    pub inbound_core_handoffs: u64,
    pub notice_core_handoffs: u64,
    pub handoff_failures: u64,
    pub handoffs_stopped: bool,
}
fn increment(n: &AtomicU64) {
    let _ = n.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_add(1))
    });
}
fn now() -> Result<i64, ()> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|t| i64::try_from(t.as_secs()).ok())
        .ok_or(())
}
fn wait(c: &Counters, d: Duration) {
    let began = Instant::now();
    while !c.stopping.load(Ordering::Acquire) && began.elapsed() < d {
        thread::sleep(Duration::from_millis(25).min(d.saturating_sub(began.elapsed())));
    }
}
fn token() -> Result<String, ()> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| ())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub struct CoreHandoffs {
    c: Arc<Counters>,
    join: Option<JoinHandle<()>>,
}
impl CoreHandoffs {
    pub fn start<M: Mailbox + Directory + Clone + Send + 'static>(
        state: SharedHub<M>,
    ) -> std::io::Result<Self> {
        let c = Arc::new(Counters::default());
        let counters = c.clone();
        let join = thread::Builder::new()
            .name("hub-core-handoffs".into())
            .spawn(move || supervise(state, counters))?;
        Ok(Self {
            c,
            join: Some(join),
        })
    }
    pub fn status(&self) -> Status {
        Status {
            inbound_core_handoffs: self.c.inbound.load(Ordering::Relaxed),
            notice_core_handoffs: self.c.notices.load(Ordering::Relaxed),
            handoff_failures: self.c.failures.load(Ordering::Relaxed),
            handoffs_stopped: self.c.stopping.load(Ordering::Relaxed),
        }
    }
    pub fn request_stop(&self) {
        self.c.stopping.store(true, Ordering::Release);
    }
    pub fn stop(&mut self) {
        self.request_stop();
        if let Some(join) = self.join.take() {
            if join.join().is_err() {
                increment(&self.c.failures);
            }
        }
    }
}
impl Drop for CoreHandoffs {
    fn drop(&mut self) {
        self.stop();
    }
}
fn supervise<M: Mailbox + Directory + Clone + Send + 'static>(
    state: SharedHub<M>,
    c: Arc<Counters>,
) {
    let mut started = HashSet::new();
    let mut joins: Vec<JoinHandle<()>> = vec![];
    while !c.stopping.load(Ordering::Acquire) {
        if joins.iter().any(JoinHandle::is_finished) {
            // Unexpected worker termination is not an unbounded respawn loop.
            increment(&c.failures);
            c.stopping.store(true, Ordering::Release);
            break;
        }
        let snapshot = match state.lock() {
            Ok(h) => h.binding_ids().map(|ids| (ids, h.backend().clone())),
            Err(_) => {
                increment(&c.failures);
                c.stopping.store(true, Ordering::Release);
                break;
            }
        };
        match snapshot {
            Ok((ids, backend)) => {
                for id in ids {
                    if started.contains(&id) {
                        continue;
                    }
                    let s = state.clone();
                    let shared = c.clone();
                    let core = backend.clone();
                    let endpoint = id.clone();
                    match thread::Builder::new()
                        .name("hub-endpoint-handoff".into())
                        .spawn(move || {
                            let mut notice = false;
                            let mut failures = 0u32;
                            while !shared.stopping.load(Ordering::Acquire) {
                                let result = step(&s, &core, &endpoint, notice, &shared);
                                notice = !notice;
                                let delay = match result {
                                    Ok(true) => {
                                        failures = 0;
                                        Duration::from_millis(100)
                                    }
                                    Ok(false) => {
                                        failures = 0;
                                        Duration::from_millis(250)
                                    }
                                    Err(()) => {
                                        increment(&shared.failures);
                                        failures = failures.saturating_add(1);
                                        Duration::from_secs(
                                            (1u64 << failures.saturating_sub(1).min(5)).min(30),
                                        )
                                    }
                                };
                                wait(&shared, delay);
                            }
                        }) {
                        Ok(join) => {
                            started.insert(id);
                            joins.push(join);
                        }
                        Err(_) => increment(&c.failures),
                    }
                }
            }
            Err(_) => increment(&c.failures),
        }
        wait(&c, Duration::from_millis(250));
    }
    for join in joins {
        if join.join().is_err() {
            increment(&c.failures);
        }
    }
}
enum Work {
    Inbound(CoreIntent),
    Notice(NoticeIntent),
}
impl Work {
    fn parts(&self) -> &[(String, String)] {
        match self {
            Self::Inbound(i) => &i.parts,
            Self::Notice(i) => &i.parts,
        }
    }
}
fn step<M: Mailbox + Directory>(
    state: &SharedHub<M>,
    backend: &M,
    endpoint: &str,
    notice: bool,
    c: &Counters,
) -> Result<bool, ()> {
    let token = token()?;
    let (work, revision) = {
        let mut hub = state.lock().map_err(|_| ())?;
        let now = now()?;
        let view = match hub.active(endpoint, now) {
            Ok(v) => v,
            Err(HubError::Binding) => return Ok(false),
            Err(_) => return Err(()),
        };
        let work = if notice {
            hub.prepare_failure_notice(endpoint, now, &token)
                .map_err(|_| ())?
                .map(Work::Notice)
        } else {
            let has_table: bool = hub
                .store
                .db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='telegram_targets')",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| ())?;
            if !has_table {
                return Ok(false);
            }
            let bound: bool = hub
                .store
                .db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM telegram_targets WHERE endpoint=?1)",
                    [endpoint],
                    |r| r.get(0),
                )
                .map_err(|_| ())?;
            if !bound {
                return Ok(false);
            }
            hub.prepare_telegram_forward(endpoint, now, &token)
                .map_err(|_| ())?
                .map(Work::Inbound)
        };
        (work, view.revision)
    };
    let Some(work) = work else {
        return Ok(false);
    };
    let mut last = String::new();
    for (key, body) in work.parts() {
        if c.stopping.load(Ordering::Acquire) {
            return Ok(false);
        }
        {
            let hub = state.lock().map_err(|_| ())?;
            match hub.active(endpoint, now()?) {
                Ok(v) if v.revision == revision && !hub.blocked().map_err(|_| ())? => {}
                _ => return Ok(false),
            }
        }
        match backend.send(endpoint, key, body, &[], None) {
            Ok(id) => last = id,
            Err(_) => {
                let mut hub = state.lock().map_err(|_| ())?;
                match &work {
                    Work::Notice(intent) => {
                        hub.fail_failure_notice(intent, now()?).map_err(|_| ())?
                    }
                    Work::Inbound(intent) => hub.fail_telegram_forward(intent).map_err(|_| ())?,
                }
                return Err(());
            }
        }
    }
    let mut hub = state.lock().map_err(|_| ())?;
    match work {
        Work::Inbound(intent) => {
            hub.complete_telegram_forward(&intent, &last)
                .map_err(|_| ())?;
            increment(&c.inbound);
        }
        Work::Notice(intent) => {
            hub.complete_failure_notice(&intent, &last, now()?)
                .map_err(|_| ())?;
            increment(&c.notices);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{AgentMetadata, Delivery},
        hub::{Binding, Hub, Readback},
        store::{Limits, Store},
        telegram::{Incoming, Target, Update},
    };
    use std::sync::{Condvar, Mutex};
    #[derive(Default)]
    struct Calls {
        entered: bool,
        released: bool,
        reject_once: bool,
        sent: Vec<(String, String)>,
    }
    #[derive(Clone, Default)]
    struct Fake {
        calls: Arc<(Mutex<Calls>, Condvar)>,
    }
    impl Directory for Fake {
        fn metadata(&self, id: &str) -> Result<AgentMetadata, crate::core::Error> {
            Ok(AgentMetadata {
                agent_id: id.into(),
                workspace_id: "ws_test".into(),
                server_id: "machine_fixture".into(),
                kind: "command".into(),
                cwd: "/fixture".into(),
                status: "running".into(),
                path: Some(format!("/team/{id}")),
            })
        }
    }
    impl Mailbox for Fake {
        fn receive(&self, _: u32) -> Result<Vec<Delivery>, crate::core::Error> {
            Ok(vec![])
        }
        fn ack(&self, _: &str) -> Result<(), crate::core::Error> {
            Ok(())
        }
        fn send(
            &self,
            to: &str,
            _: &str,
            body: &str,
            contexts: &[String],
            external: Option<serde_json::Value>,
        ) -> Result<String, crate::core::Error> {
            assert!(contexts.is_empty());
            assert!(external.is_none());
            let (mutex, signal) = &*self.calls;
            let mut calls = mutex.lock().unwrap();
            if to == "ag_blocked" && !calls.released {
                calls.entered = true;
                signal.notify_all();
                let (c, wait) = signal
                    .wait_timeout_while(calls, Duration::from_secs(3), |c| !c.released)
                    .unwrap();
                calls = c;
                if wait.timed_out() {
                    return Err(crate::core::Error::Timeout);
                }
            }
            if calls.reject_once {
                calls.reject_once = false;
                return Err(crate::core::Error::Rejected);
            }
            calls.sent.push((to.into(), body.into()));
            Ok(format!("msg_fixture{}", calls.sent.len()))
        }
    }
    fn fixture(body: &str) -> (SharedHub<Fake>, Fake) {
        let fake = Fake::default();
        let mut hub = Hub::new(
            Store::memory(Limits::default()).unwrap(),
            fake.clone(),
            "app_hub",
            "ws_test",
        )
        .unwrap();
        let time = now().unwrap();
        for (id, chat) in [("ag_blocked", 10), ("ag_fast", 20)] {
            hub.add_binding_audited(
                Binding {
                    endpoint_id: id.into(),
                    generation: 1,
                    recipient: "ag_recipient".into(),
                    expected_path: format!("/team/{id}"),
                    expected_cwd: "/fixture".into(),
                    expected_server_id: "machine_fixture".into(),
                },
                "user_owner",
                time,
            )
            .unwrap();
            hub.bind_telegram_audited(
                id,
                1,
                Target {
                    account_id: "bot_fixture".into(),
                    chat_id: chat,
                    topic_id: None,
                    allowed_senders: vec![9],
                },
                "user_owner",
                time,
            )
            .unwrap();
            hub.resume_after_readback_audited(
                id,
                2,
                Readback {
                    observed: Some(fake.metadata(id).unwrap()),
                    requested_at: time,
                },
                "user_owner",
                time,
            )
            .unwrap();
        }
        hub.admit_telegram(
            "bot_fixture",
            vec![
                Update {
                    update_id: 1,
                    message: Some(Incoming {
                        message_id: 1,
                        chat_id: 10,
                        topic_id: None,
                        sender_id: 9,
                        text: body.into(),
                    }),
                },
                Update {
                    update_id: 2,
                    message: Some(Incoming {
                        message_id: 2,
                        chat_id: 20,
                        topic_id: None,
                        sender_id: 9,
                        text: "fast message".into(),
                    }),
                },
            ],
            time,
        )
        .unwrap();
        (Arc::new(Mutex::new(hub)), fake)
    }
    fn wait_for(fake: &Fake, predicate: impl Fn(&Calls) -> bool) {
        let (mutex, signal) = &*fake.calls;
        let calls = mutex.lock().unwrap();
        let (_calls, result) = signal
            .wait_timeout_while(calls, Duration::from_secs(2), |c| !predicate(c))
            .unwrap();
        assert!(
            !result.timed_out(),
            "owned worker did not reach expected state"
        );
    }
    fn release(fake: &Fake) {
        let (mutex, signal) = &*fake.calls;
        mutex.lock().unwrap().released = true;
        signal.notify_all();
    }
    #[test]
    fn slow_core_send_does_not_hold_actor_or_other_endpoint_and_stop_joins_only_own_threads() {
        let (state, fake) = fixture("blocked message");
        let mut workers = CoreHandoffs::start(state.clone()).unwrap();
        wait_for(&fake, |c| c.entered);
        // A blocked Core send owns no state mutex. Another endpoint proceeds.
        let actor_deadline = Instant::now() + Duration::from_millis(500);
        while state.try_lock().is_err() {
            assert!(
                Instant::now() < actor_deadline,
                "network send held the state actor"
            );
            thread::sleep(Duration::from_millis(2));
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fake
            .calls
            .0
            .lock()
            .unwrap()
            .sent
            .iter()
            .any(|(id, _)| id == "ag_fast")
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        release(&fake);
        let deadline = Instant::now() + Duration::from_secs(2);
        while workers.status().inbound_core_handoffs != 2 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        workers.stop();
        assert_eq!(workers.status().handoff_failures, 0);
        for (endpoint, wire) in &fake.calls.0.lock().unwrap().sent {
            let fragment = crate::protocol::Fragment::parse(wire).unwrap();
            assert!(endpoint == "ag_blocked" || endpoint == "ag_fast");
            assert_eq!(fragment.generation, 1);
            assert!(fragment.direction == crate::protocol::Direction::Inbound);
        }
    }
    #[test]
    fn actual_inbound_send_errors_pause_at_eight_despite_idle_notice_turns() {
        let (state, fake) = fixture("inbound");
        let counters = Counters::default();
        for n in 0..8 {
            fake.calls.0.lock().unwrap().reject_once = true;
            state
                .lock()
                .unwrap()
                .store
                .db
                .execute(
                    "UPDATE inbound SET lease_until=0 WHERE endpoint='ag_fast'",
                    [],
                )
                .unwrap();
            assert!(step(&state, &fake, "ag_fast", false, &counters).is_err());
            assert!(!step(&state, &fake, "ag_fast", true, &counters).unwrap());
            assert_eq!(
                state.lock().unwrap().view("ag_fast").unwrap().paused,
                n == 7
            );
        }
        assert!(!step(&state, &fake, "ag_fast", false, &counters).unwrap());
        let hub = state.lock().unwrap();
        assert_eq!(hub.view("ag_fast").unwrap().reason, "core_handoff_failing");
        assert_eq!(
            hub.last_core_handoff_failure().unwrap(),
            Some("core_send_unavailable")
        );
        assert!(fake.calls.0.lock().unwrap().sent.is_empty());
        assert!(!hub.view("ag_blocked").unwrap().paused);
    }
    #[test]
    fn failure_notice_core_error_keeps_durable_retry_and_success_marks_handoff_only() {
        let (state, fake) = fixture("inbound");
        let time = now().unwrap();
        {
            let mut hub = state.lock().unwrap();
            let binding = hub.view("ag_fast").unwrap().binding;
            let envelope = crate::protocol::Envelope::Outbound {
                delivery_id: "delivery_fixture".into(),
                message_id: "msg_original".into(),
                original_sender: crate::protocol::Sender {
                    kind: crate::protocol::SenderKind::Agent,
                    id: "ag_recipient".into(),
                },
                body: "PRIVATE ORIGINAL BODY".into(),
                context_ids: vec![],
            };
            let tx = hub.store.db.transaction().unwrap();
            crate::notices::insert_notice(
                &tx,
                &binding,
                &envelope,
                &"a".repeat(64),
                crate::protocol::NoticeCode::TransportRejected,
                time,
            )
            .unwrap();
            tx.commit().unwrap();
        }
        fake.calls.0.lock().unwrap().reject_once = true;
        let counters = Counters::default();
        assert!(step(&state, &fake, "ag_fast", true, &counters).is_err());
        assert!(fake.calls.0.lock().unwrap().sent.is_empty());
        {
            let hub = state.lock().unwrap();
            let status:(i64,i64,i64)=hub.store.db.query_row("SELECT state,handoff_attempts,next_attempt FROM hub_failure_notices WHERE endpoint='ag_fast'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            assert_eq!((status.0, status.1), (0, 1));
            assert!(status.2 > time);
            // Deterministic passage of the retry deadline, no wall-clock sleep.
            hub.store
                .db
                .execute(
                    "UPDATE hub_failure_notices SET next_attempt=0 WHERE endpoint='ag_fast'",
                    [],
                )
                .unwrap();
        }
        assert!(step(&state, &fake, "ag_fast", true, &counters).unwrap());
        assert_eq!(counters.notices.load(Ordering::Relaxed), 1);
        let calls = fake.calls.0.lock().unwrap();
        assert_eq!(calls.sent.len(), 1);
        assert_eq!(calls.sent[0].0, "ag_fast");
        let fragment = crate::protocol::Fragment::parse(&calls.sent[0].1).unwrap();
        let mut assembly = crate::protocol::Assembly::new(
            &fragment,
            "ag_fast",
            1,
            crate::protocol::Direction::Inbound,
        )
        .unwrap();
        let envelope = assembly.push(&fragment).unwrap().unwrap();
        let body = envelope.failure_notice_body().unwrap();
        assert!(body.contains("transport_rejected"));
        assert!(!body.contains("PRIVATE ORIGINAL BODY"));
        assert_eq!(
            state
                .lock()
                .unwrap()
                .failure_notices("ag_fast", 10)
                .unwrap()[0]
                .status,
            "handed_to_endpoint"
        );
    }
    #[test]
    fn owner_pause_between_parts_prevents_the_next_send_and_does_not_complete_partial_job() {
        let (state, fake) = fixture(&"x".repeat(crate::protocol::MAX_BODY));
        let counters = Arc::new(Counters::default());
        let s = state.clone();
        let backend = fake.clone();
        let c = counters.clone();
        let join = thread::spawn(move || step(&s, &backend, "ag_blocked", false, &c));
        wait_for(&fake, |c| c.entered);
        state
            .lock()
            .unwrap()
            .pause_audited("ag_blocked", 3, "user_owner", now().unwrap())
            .unwrap();
        release(&fake);
        assert!(!join.join().unwrap().unwrap());
        assert_eq!(fake.calls.0.lock().unwrap().sent.len(), 1);
        assert_eq!(counters.inbound.load(Ordering::Relaxed), 0);
        assert!(state.lock().unwrap().view("ag_blocked").unwrap().paused);
    }
}
