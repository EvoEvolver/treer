//! Email runtime defaults to private outbox; Proxy transport requires explicit configuration.
//! All Core/directory calls and private file publication happen outside state.
use crate::{
    core::Core,
    email::{self, proxy::Sender, Contact, Queue},
    hub::{Directory, Readback},
    relay::Mailbox,
    scheduler::SharedHub,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub outbox_directory: PathBuf,
    #[serde(default)]
    pub mode: email::Mode,
}
impl Config {
    pub(crate) fn validate(&self) -> Result<(), ()> {
        crate::private_store::open_directory(&self.outbox_directory, true)
            .map(|_| ())
            .map_err(|_| ())
    }
}
#[derive(Default)]
struct State {
    stop: AtomicBool,
    stop_timed_out: AtomicBool,
    published: AtomicU64,
    replied: AtomicU64,
    failures: AtomicU64,
}
#[derive(Serialize)]
pub struct Status {
    pub mode: &'static str,
    pub published_outbox: u64,
    pub core_replies: u64,
    pub failures: u64,
    pub stopped: bool,
    pub stop_timed_out: bool,
}
#[derive(Serialize)]
pub struct Snapshot {
    pub mode: &'static str,
    pub worker: Option<Status>,
    pub targets: Vec<Target>,
    pub targets_truncated: bool,
}
#[derive(Serialize)]
pub struct Target {
    pub endpoint_id: String,
    pub generation: i64,
    pub address: String,
    pub member_id: String,
    pub revoked: bool,
}
impl Snapshot {
    pub(crate) fn read(
        store: &crate::store::Store,
        worker: Option<&Workers>,
        mode: email::Mode,
    ) -> Result<Self, ()> {
        let mut q=store.db.prepare("SELECT t.endpoint,t.generation,t.address,t.member,t.revoked FROM email_targets t JOIN endpoints e ON e.id=t.endpoint AND e.generation=t.generation ORDER BY t.endpoint LIMIT 21").map_err(|_|())?;
        let mut targets = q
            .query_map([], |r| {
                Ok(Target {
                    endpoint_id: r.get(0)?,
                    generation: r.get(1)?,
                    address: r.get(2)?,
                    member_id: r.get(3)?,
                    revoked: r.get(4)?,
                })
            })
            .map_err(|_| ())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ())?;
        let targets_truncated = targets.len() > 20;
        targets.truncate(20);
        Ok(Self {
            mode: match mode {
                email::Mode::Outbox => "outbox",
                email::Mode::Proxy => "proxy",
            },
            worker: worker.map(Workers::status),
            targets,
            targets_truncated,
        })
    }
}
pub struct Workers {
    state: Arc<State>,
    join: Option<JoinHandle<()>>,
    startup: Option<StartupLease<Core>>,
    mode: email::Mode,
}

/// Per-Hub worker ownership. Private-store process exclusion is separate.
/// Acquire/recover under the state mutex before any worker thread is spawned.
struct StartupLease<M> {
    hub: SharedHub<M>,
    release_on_drop: bool,
}
impl<M> StartupLease<M> {
    fn acquire(hub: &SharedHub<M>) -> Result<Self, ()> {
        let mut h = hub.lock().map_err(|_| ())?;
        if h.email_workers_active {
            return Err(());
        }
        h.store.recheck_private_storage().map_err(|_| ())?;
        Queue::new(&mut h.store)
            .map_err(|_| ())?
            .recover_interrupted_proxy_deliveries()
            .map_err(|_| ())?;
        h.email_workers_active = true;
        Ok(Self {
            hub: hub.clone(),
            release_on_drop: true,
        })
    }
}
impl<M> Drop for StartupLease<M> {
    fn drop(&mut self) {
        if self.release_on_drop {
            if let Ok(mut h) = self.hub.lock() {
                h.email_workers_active = false;
            }
        }
    }
}
fn clock() -> Result<i64, ()> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok())
        .ok_or(())
}
fn nonce() -> Result<String, ()> {
    let mut b = [0; 32];
    getrandom::fill(&mut b).map_err(|_| ())?;
    Ok(b.iter().map(|b| format!("{b:02x}")).collect())
}
fn increment(n: &AtomicU64) {
    let _ = n.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_add(1))
    });
}
fn wait(s: &State) {
    for _ in 0..20 {
        if s.stop.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
}
impl Workers {
    pub fn start(
        hub: SharedHub<Core>,
        core: Core,
        directory: Arc<Sender>,
        config: Config,
        public_url: String,
    ) -> std::io::Result<Self> {
        config
            .validate()
            .map_err(|_| std::io::Error::other("email_private_outbox_unavailable"))?;
        let mode = config.mode;
        let startup = StartupLease::acquire(&hub)
            .map_err(|_| std::io::Error::other("email_startup_recovery_unavailable"))?;
        let state = Arc::new(State::default());
        let shared = state.clone();
        let join = thread::Builder::new()
            .name("hub-email-supervisor".into())
            .spawn(move || {
                let mut started = HashSet::new();
                let mut joins: Vec<JoinHandle<()>> = vec![];
                while !shared.stop.load(Ordering::Acquire) {
                    if joins.iter().any(JoinHandle::is_finished) {
                        increment(&shared.failures);
                        shared.stop.store(true, Ordering::Release);
                        break;
                    }
                    if hub.lock().ok().and_then(|mut h| {
                        email::Queue::new(&mut h.store)
                            .ok()
                            .map(|mut q| q.purge_retired_outbox(&config.outbox_directory).is_ok())
                    }) != Some(true)
                    {
                        increment(&shared.failures);
                    }
                    let ids = hub
                        .lock()
                        .ok()
                        .and_then(|h| h.binding_ids().ok())
                        .unwrap_or_default();
                    for id in ids {
                        if started.contains(&id) {
                            continue;
                        }
                        let (h, c, d, s, cfg, url) = (
                            hub.clone(),
                            core.clone(),
                            directory.clone(),
                            shared.clone(),
                            config.clone(),
                            public_url.clone(),
                        );
                        let endpoint = id.clone();
                        match thread::Builder::new()
                            .name("hub-email-endpoint".into())
                            .spawn(move || {
                                while !s.stop.load(Ordering::Acquire) {
                                    if step(&h, &c, &endpoint, &cfg, &url, &d, &s).is_err() {
                                        increment(&s.failures);
                                    }
                                    wait(&s);
                                }
                            }) {
                            Ok(j) => {
                                started.insert(id);
                                joins.push(j);
                            }
                            Err(_) => {
                                increment(&shared.failures);
                                shared.stop.store(true, Ordering::Release);
                                break;
                            }
                        }
                    }
                    wait(&shared);
                }
                for j in joins {
                    if j.join().is_err() {
                        increment(&shared.failures);
                    }
                }
            })?;
        Ok(Self {
            state,
            join: Some(join),
            startup: Some(startup),
            mode,
        })
    }
    pub fn status(&self) -> Status {
        Status {
            mode: match self.mode {
                email::Mode::Outbox => "outbox",
                email::Mode::Proxy => "proxy",
            },
            published_outbox: self.state.published.load(Ordering::Relaxed),
            core_replies: self.state.replied.load(Ordering::Relaxed),
            failures: self.state.failures.load(Ordering::Relaxed),
            stopped: self.state.stop.load(Ordering::Acquire),
            stop_timed_out: self.state.stop_timed_out.load(Ordering::Acquire),
        }
    }
    pub fn request_stop(&self) {
        self.state.stop.store(true, Ordering::Release);
    }
    pub fn stop(&mut self) {
        self.stop_with_deadline(Duration::from_secs(60));
    }
    fn stop_with_deadline(&mut self, budget: Duration) {
        self.request_stop();
        let mut joined = true;
        if let Some(j) = self.join.take() {
            let deadline = std::time::Instant::now() + budget;
            while !j.is_finished() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if !j.is_finished() {
                self.state.stop_timed_out.store(true, Ordering::Release);
                increment(&self.state.failures);
                joined = false; // Keep startup latched: detached jobs may still commit.
            } else if j.join().is_err() {
                increment(&self.state.failures);
                joined = false;
            }
        }
        if let Some(mut startup) = self.startup.take() {
            // A panicked supervisor may have detached endpoint threads. Never
            // recover their in-flight reservations as if they had been joined.
            startup.release_on_drop = joined;
        }
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        self.stop();
    }
}
fn refresh<M: Mailbox + Directory>(hub: &SharedHub<M>, core: &M, id: &str) -> Result<(), ()> {
    let revision = hub
        .lock()
        .map_err(|_| ())?
        .view(id)
        .map_err(|_| ())?
        .revision;
    let requested_at = clock()?;
    let observed = core.metadata(id).ok();
    hub.lock()
        .map_err(|_| ())?
        .record_readback(
            id,
            revision,
            Readback {
                observed,
                requested_at,
            },
            clock()?,
        )
        .map_err(|_| ())
}
fn fence<M: Mailbox + Directory>(
    hub: &SharedHub<M>,
    id: &str,
    contact: &Contact,
) -> Result<(), ()> {
    let h = hub.lock().map_err(|_| ())?;
    h.store.recheck_private_storage().map_err(|_| ())?;
    h.active(id, clock()?).map_err(|_| ())?;
    if h.blocked().map_err(|_| ())?
        || email::contact(&h.store, id).map_err(|_| ())?.as_ref() != Some(contact)
    {
        return Err(());
    }
    Ok(())
}
fn capacity_available<M>(
    hub: &mut crate::hub::Hub<M>,
    endpoint: &str,
    generation: i64,
) -> Result<bool, ()> {
    if Queue::new(&mut hub.store)
        .map_err(|_| ())?
        .proxy_delivery_status(endpoint)
        .map_err(|_| ())?
        .at_capacity
    {
        contain_capacity(hub, endpoint, generation)?;
        return Ok(false);
    }
    Ok(true)
}
fn contain_capacity<M>(
    hub: &mut crate::hub::Hub<M>,
    endpoint: &str,
    generation: i64,
) -> Result<(), ()> {
    let tx = hub
        .store
        .db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| ())?;
    if tx
        .execute(
            "UPDATE endpoints SET paused=1 WHERE id=?1 AND generation=?2 AND paused=0",
            rusqlite::params![endpoint, generation],
        )
        .map_err(|_| ())?
        == 1
    {
        tx.execute("UPDATE hub_bindings SET revision=revision+1,reason='email_capacity_full' WHERE endpoint=?1",[endpoint]).map_err(|_|())?;
    }
    tx.commit().map_err(|_| ())
}
fn step(
    hub: &SharedHub<Core>,
    core: &Core,
    id: &str,
    config: &Config,
    url: &str,
    directory: &Sender,
    state: &State,
) -> Result<(), ()> {
    let (contact, generation, work) = {
        let mut h = hub.lock().map_err(|_| ())?;
        let Some(contact) = email::contact(&h.store, id).map_err(|_| ())? else {
            return Ok(());
        };
        let Ok(view) = h.active(id, clock()?) else {
            return Ok(());
        };
        if config.mode == email::Mode::Proxy
            && !capacity_available(&mut h, id, view.binding.generation)?
        {
            return Ok(());
        }
        Queue::new(&mut h.store).map_err(|_| ())?;
        let pending:bool=h.store.db.query_row("SELECT EXISTS(SELECT 1 FROM inbound WHERE endpoint=?1 AND generation=?2 AND pending=1) OR EXISTS(SELECT 1 FROM email_intents WHERE endpoint=?1 AND generation=?2 AND state=0 AND next_try<=?3)",rusqlite::params![id,view.binding.generation,clock()?],|r|r.get(0)).map_err(|_|())?;
        let work = pending || !h.ready_outbound(id, clock()?).map_err(|_| ())?.is_empty();
        (contact, view.binding.generation, work)
    };
    if !work || state.stop.load(Ordering::Acquire) {
        return Ok(());
    }
    if !directory
        .current_member(core, &contact.member_id)
        .unwrap_or(false)
    {
        return Err(());
    }
    refresh(hub, core, id)?;
    fence(hub, id, &contact)?;
    // Admit one reassembled Agent message. A frozen email intent is durable before
    // the Core transfer can be completed; a replay checks the same payload digest.
    let outbound = {
        hub.lock()
            .map_err(|_| ())?
            .ready_outbound(id, clock()?)
            .map_err(|_| ())?
            .into_iter()
            .next()
    };
    if let Some((transfer, envelope)) = outbound {
        let mut h = hub.lock().map_err(|_| ())?;
        let mut q = Queue::new(&mut h.store).map_err(|_| ())?;
        let admitted = q.admit(id, generation, &envelope, clock()?);
        match admitted {
            Ok(_) => {
                let crate::protocol::Envelope::Outbound { message_id, .. } = envelope else {
                    return Err(());
                };
                let receipt = q.receipt(id, generation, &message_id).map_err(|_| ())?;
                if receipt == Some(1) {
                    h.store
                        .finish_transfer(
                            id,
                            generation,
                            crate::protocol::Direction::Outbound,
                            &transfer,
                            clock()?,
                        )
                        .map_err(|_| ())?;
                } else if receipt == Some(2) {
                    h.park_transport(
                        id,
                        generation,
                        &transfer,
                        crate::failures::FailureReason::TransportRetriesExhausted,
                        clock()?,
                    )
                    .map_err(|_| ())?;
                }
            }
            Err(email::Error::Invalid | email::Error::Denied | email::Error::Conflict) => {
                h.park_transport(
                    id,
                    generation,
                    &transfer,
                    crate::failures::FailureReason::TransportRejected,
                    clock()?,
                )
                .map_err(|_| ())?;
            }
            Err(_) => return Err(()),
        }
    }
    let intent = {
        let mut h = hub.lock().map_err(|_| ())?;
        let mut queue = Queue::new(&mut h.store).map_err(|_| ())?;
        let claimed = match config.mode {
            email::Mode::Outbox => queue.claim(id, generation, clock()?, &nonce()?),
            email::Mode::Proxy => queue.claim_proxy(id, generation, clock()?, &nonce()?),
        };
        match claimed {
            Ok(v) => v,
            Err(email::Error::Rate | email::Error::Lease) => None,
            Err(_) => return Err(()),
        }
    };
    if let Some(intent) = intent {
        fence(hub, id, &contact)?;
        let dispatch = {
            let mut h = hub.lock().map_err(|_| ())?;
            config.mode == email::Mode::Proxy
                || Queue::new(&mut h.store)
                    .map_err(|_| ())?
                    .dispatchable(&intent, clock()?)
                    .map_err(|_| ())?
        };
        if !dispatch || state.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        match config.mode {
            email::Mode::Outbox => {
                let result = email::write_outbox(&intent, url, &config.outbox_directory);
                let mut h = hub.lock().map_err(|_| ())?;
                let mut q = Queue::new(&mut h.store).map_err(|_| ())?;
                if result.is_ok() {
                    q.complete(&intent, email::Mode::Outbox).map_err(|_| ())?;
                    increment(&state.published);
                } else {
                    q.failed(&intent, clock()?, false).map_err(|_| ())?;
                    return Err(());
                }
            }
            email::Mode::Proxy => {
                // Preparation is durable but still abandonable. Reservation is
                // committed before obtaining the one-use transport capability.
                let attempt = {
                    let mut h = hub.lock().map_err(|_| ())?;
                    let mut q = Queue::new(&mut h.store).map_err(|_| ())?;
                    let prepared = q.prepare_proxy_delivery(&intent, url, clock()?);
                    let reserved = prepared.and_then(|()| q.begin_proxy_delivery_now(&intent, url));
                    match reserved {
                        Ok(attempt) => attempt,
                        Err(email::Error::Full) => {
                            contain_capacity(&mut h, id, generation)?;
                            return Err(());
                        }
                        Err(_) => return Err(()),
                    }
                };
                let completion = directory.send_committed(core, attempt, || {
                    // Token mint may have blocked while membership or Core pins
                    // changed. Revalidate remotely outside the Hub mutex first.
                    // A consumed reservation stays uncertain on any refusal.
                    if state.stop.load(Ordering::Acquire)
                        || !directory
                            .current_member(core, &contact.member_id)
                            .unwrap_or(false)
                        || refresh(hub, core, id).is_err()
                    {
                        return false;
                    }
                    // Remote checks may themselves have blocked. Only now take
                    // the current local pause/contact/generation/lease/stop fence.
                    if state.stop.load(Ordering::Acquire) {
                        return false;
                    }
                    let Ok(mut h) = hub.lock() else {
                        return false;
                    };
                    let Ok(now) = clock() else {
                        return false;
                    };
                    if state.stop.load(Ordering::Acquire)
                        || h.active(id, now).is_err()
                        || h.blocked().unwrap_or(true)
                        || email::contact(&h.store, id).ok().flatten().as_ref() != Some(&contact)
                    {
                        return false;
                    }
                    Queue::new(&mut h.store)
                        .is_ok_and(|mut q| q.proxy_dispatch_allowed_now(&intent, url))
                });
                // Persist even if stop/pause occurred while HTTP was in flight.
                let mut h = hub.lock().map_err(|_| ())?;
                completion
                    .commit(&mut Queue::new(&mut h.store).map_err(|_| ())?)
                    .map_err(|_| ())?;
            }
        }
    }
    let inbound = {
        let mut h = hub.lock().map_err(|_| ())?;
        h.prepare_email_forward(id, clock()?, &nonce()?)
            .map_err(|_| ())?
    };
    if let Some(intent) = inbound {
        let mut last = String::new();
        for (key, body) in &intent.parts {
            if state.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            // Revalidate a contact before each actual send. No state mutex spans I/O.
            if !directory
                .current_member(core, &contact.member_id)
                .unwrap_or(false)
            {
                return Err(());
            }
            refresh(hub, core, id)?;
            fence(hub, id, &contact)?;
            let leased:bool=hub.lock().map_err(|_|())?.store.db.query_row("SELECT EXISTS(SELECT 1 FROM inbound WHERE id=?1 AND lease_token=?2 AND pending=1 AND lease_until>?3)",rusqlite::params![intent.job,intent.token,clock()?],|r|r.get(0)).map_err(|_|())?;
            if !leased {
                return Ok(());
            }
            match core.send(id, key, body, &[], None) {
                Ok(v) => last = v,
                Err(_) => {
                    hub.lock()
                        .map_err(|_| ())?
                        .fail_telegram_forward(&intent, clock()?)
                        .map_err(|_| ())?;
                    return Err(());
                }
            }
        }
        hub.lock()
            .map_err(|_| ())?
            .complete_telegram_forward(&intent, &last)
            .map_err(|_| ())?;
        increment(&state.replied);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        core::{AgentMetadata, Delivery},
        hub::Hub,
        protocol::{Envelope, Sender as Principal, SenderKind},
        store::{Limits, Store},
    };
    use std::sync::Mutex;

    struct NoIo;
    impl Directory for NoIo {
        fn metadata(&self, _: &str) -> Result<AgentMetadata, crate::core::Error> {
            panic!("startup containment must not call Core");
        }
    }
    impl Mailbox for NoIo {
        fn receive(&self, _: u32) -> Result<Vec<Delivery>, crate::core::Error> {
            panic!("startup containment must not receive");
        }
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<serde_json::Value>,
        ) -> Result<String, crate::core::Error> {
            panic!("startup containment must not send");
        }
        fn ack(&self, _: &str) -> Result<(), crate::core::Error> {
            panic!("startup containment must not acknowledge");
        }
    }
    /// Cross-module composition fixture: invoke the actual exclusive startup
    /// recovery guard, without starting workers or making any Core call.
    pub(crate) fn recover_private_store_with_startup_lease(store: Store) -> Store {
        let hub = Arc::new(std::sync::Mutex::new(
            Hub::new(store, NoIo, "ag_hub", "ws_test").unwrap(),
        ));
        let lease = StartupLease::acquire(&hub).unwrap();
        drop(lease);
        match Arc::try_unwrap(hub) {
            Ok(mutex) => mutex.into_inner().unwrap().store,
            Err(_) => panic!("startup lease retained the store"),
        }
    }
    fn fixture() -> SharedHub<NoIo> {
        fixture_state("dispatching")
    }
    fn fixture_state(initial: &str) -> SharedHub<NoIo> {
        let mut h = Hub::new(
            Store::memory(Limits::default()).unwrap(),
            NoIo,
            "ag_hub",
            "ws_test",
        )
        .unwrap();
        h.store
            .register_paused("ag_email", 1, "ag_internal")
            .unwrap();
        Queue::new(&mut h.store)
            .unwrap()
            .bind_paused(
                "ag_email",
                1,
                Contact {
                    address: email::REAL_RECIPIENT.into(),
                    member_id: "user_alice".into(),
                },
            )
            .unwrap();
        h.store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        let mut q = Queue::new(&mut h.store).unwrap();
        let now = clock().unwrap();
        q.admit(
            "ag_email",
            1,
            &Envelope::Outbound {
                delivery_id: "del_test".into(),
                message_id: "msg_test".into(),
                original_sender: Principal {
                    kind: SenderKind::Agent,
                    id: "ag_internal".into(),
                },
                body: "synthetic summary".into(),
                context_ids: vec![],
            },
            now,
        )
        .unwrap();
        let intent = q
            .claim_proxy("ag_email", 1, now, &"a".repeat(64))
            .unwrap()
            .unwrap();
        q.prepare_proxy_delivery(&intent, "https://hub.example/", now)
            .unwrap();
        if initial != "prepared" {
            let attempt = q
                .begin_proxy_delivery_now(&intent, "https://hub.example/")
                .unwrap();
            match initial {
                "dispatching" => drop(attempt),
                "provider_accepted" => q
                    .finish_proxy_delivery(
                        attempt,
                        crate::email::delivery::Outcome::ProviderAccepted,
                    )
                    .unwrap(),
                "uncertain" => q
                    .finish_proxy_delivery(attempt, crate::email::delivery::Outcome::Uncertain)
                    .unwrap(),
                _ => panic!("unknown fixture state"),
            }
        }
        Arc::new(Mutex::new(h))
    }
    fn state(hub: &SharedHub<NoIo>) -> String {
        hub.lock()
            .unwrap()
            .store
            .db
            .query_row("SELECT state FROM email_dispatch", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn uncertainty_capacity_pauses_binding_once_without_releasing_or_posting() {
        let shared = fixture_state("uncertain");
        let mut h = shared.lock().unwrap();
        let binding = crate::hub::Binding {
            endpoint_id: "ag_email".into(),
            generation: 1,
            recipient: "ag_internal".into(),
            expected_path: "/team/email".into(),
            expected_cwd: "/workspace/email".into(),
            expected_server_id: "machine_fixture".into(),
        };
        h.store
            .db
            .execute(
                "INSERT INTO hub_bindings(endpoint,definition) VALUES('ag_email',?1)",
                [serde_json::to_string(&binding).unwrap()],
            )
            .unwrap();
        h.store.db.execute_batch("WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<31) INSERT INTO email_intents(id,thread,endpoint,generation,state) SELECT 'capacity_'||n.v,i.thread,i.endpoint,i.generation,0 FROM n CROSS JOIN (SELECT * FROM email_intents LIMIT 1) i;
        INSERT INTO email_dispatch(id,endpoint,generation,payload,state) SELECT i.id,i.endpoint,i.generation,d.payload,'uncertain' FROM email_intents i CROSS JOIN (SELECT payload FROM email_dispatch LIMIT 1) d WHERE i.id LIKE 'capacity_%';").unwrap();
        assert!(!capacity_available(&mut h, "ag_email", 1).unwrap());
        let view = h.view("ag_email").unwrap();
        assert!(view.paused);
        assert_eq!(view.reason, "email_capacity_full");
        assert_eq!(view.revision, 2);
        assert!(!capacity_available(&mut h, "ag_email", 1).unwrap());
        assert_eq!(h.view("ag_email").unwrap().revision, 2);
        assert_eq!(
            Queue::new(&mut h.store)
                .unwrap()
                .proxy_delivery_status("ag_email")
                .unwrap()
                .uncertain,
            32
        );
    }
    #[test]
    fn startup_recovers_before_ownership_and_duplicate_start_cannot_recover_live_work() {
        let hub = fixture();
        assert_eq!(state(&hub), "dispatching");
        let lease = StartupLease::acquire(&hub).unwrap();
        assert_eq!(state(&hub), "uncertain");
        assert!(hub.lock().unwrap().email_workers_active);
        // Simulate a later in-flight attempt while this supervisor owns workers.
        hub.lock()
            .unwrap()
            .store
            .db
            .execute("UPDATE email_dispatch SET state='dispatching'", [])
            .unwrap();
        assert!(StartupLease::acquire(&hub).is_err());
        assert_eq!(state(&hub), "dispatching");
        drop(lease); // same release used only after workers are joined
        assert!(!hub.lock().unwrap().email_workers_active);
        let _restarted = StartupLease::acquire(&hub).unwrap();
        assert_eq!(state(&hub), "uncertain");
    }

    #[test]
    fn clean_restart_preserves_prepared_accepted_and_uncertain_rows() {
        for initial in ["prepared", "provider_accepted", "uncertain"] {
            let hub = fixture_state(initial);
            let before: (Vec<u8>, i64) = {
                let h = hub.lock().unwrap();
                h.store.db.query_row("SELECT d.payload,i.state FROM email_dispatch d JOIN email_intents i ON i.id=d.id", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap()
            };
            for _ in 0..2 {
                let lease = StartupLease::acquire(&hub).unwrap();
                assert_eq!(state(&hub), initial);
                let after: (Vec<u8>, i64) = {
                    let h = hub.lock().unwrap();
                    h.store.db.query_row("SELECT d.payload,i.state FROM email_dispatch d JOIN email_intents i ON i.id=d.id", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap()
                };
                assert_eq!(after, before);
                drop(lease); // confirmed clean stop between worker generations
            }
        }
    }

    #[test]
    fn failed_startup_commit_never_grants_worker_ownership() {
        let hub = fixture();
        hub.lock().unwrap().store.db.commit_hook(Some(|| true));
        assert!(StartupLease::acquire(&hub).is_err());
        assert!(!hub.lock().unwrap().email_workers_active);
        assert_eq!(state(&hub), "dispatching");
        hub.lock()
            .unwrap()
            .store
            .db
            .commit_hook(None::<fn() -> bool>);
        let _lease = StartupLease::acquire(&hub).unwrap();
        assert_eq!(state(&hub), "uncertain");
    }

    #[test]
    fn unconfirmed_worker_stop_latches_ownership_instead_of_recovering() {
        let hub = fixture();
        let mut lease = StartupLease::acquire(&hub).unwrap();
        lease.release_on_drop = false; // supervisor join returned Err
        drop(lease);
        assert!(hub.lock().unwrap().email_workers_active);
        assert!(StartupLease::acquire(&hub).is_err());
    }
    #[test]
    fn bounded_stop_returns_without_releasing_unjoined_startup_ownership() {
        let h = Arc::new(Mutex::new(
            Hub::new(
                Store::memory(Limits::default()).unwrap(),
                Core::new(std::path::Path::new("/usr/bin/false"), "ag_hub").unwrap(),
                "ag_hub",
                "ws_test",
            )
            .unwrap(),
        ));
        let lease = StartupLease::acquire(&h).unwrap();
        let (send, recv) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel();
        let held = h.clone();
        let j = std::thread::spawn(move || {
            recv.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(held);
            done.send(()).unwrap();
        });
        let mut worker = Workers {
            state: Arc::new(State::default()),
            join: Some(j),
            startup: Some(lease),
            mode: email::Mode::Proxy,
        };
        let before = std::time::Instant::now();
        worker.stop_with_deadline(Duration::ZERO);
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(worker.status().stop_timed_out);
        assert!(StartupLease::acquire(&h).is_err());
        send.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(worker); // does not wait a second deadline or clear the latch
        assert!(h.lock().unwrap().email_workers_active);
    }
}
