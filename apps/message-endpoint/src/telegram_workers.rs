//! Account-isolated Telegram loops. Tokens are private startup configuration,
//! never a settings response, Core message, URL parameter, or log field.
use crate::{
    hub::Directory,
    relay::Mailbox,
    scheduler::SharedHub,
    telegram::{Error, Transport, Update},
    telegram_http::BotClient,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const VERIFY_INTERVAL: Duration = Duration::from_secs(300);
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub account_id: String,
    pub bot_id: i64,
    pub token_file: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretFile {
    token: String,
}
pub trait AccountTransport: Transport + Send + Sync + 'static {
    fn verify(&self) -> Result<(), Error>;
    fn poll(&self, offset: i64) -> Result<Vec<Update>, Error>;
}
impl AccountTransport for BotClient {
    fn verify(&self) -> Result<(), Error> {
        BotClient::verify(self)
    }
    fn poll(&self, offset: i64) -> Result<Vec<Update>, Error> {
        BotClient::poll(self, offset)
    }
}
pub struct Account {
    id: String,
    bot_id: i64,
    client: Arc<dyn AccountTransport>,
    health: Mutex<Health>,
    polls: AtomicU64,
    admitted: AtomicU64,
    sent: AtomicU64,
    failures: AtomicU64,
}
struct Health {
    phase: &'static str,
    verified_at: Option<Instant>,
    retry_at: Option<Instant>,
    fatal: bool,
    errors: u32,
}
impl Default for Health {
    fn default() -> Self {
        Self {
            phase: "unverified",
            verified_at: None,
            retry_at: None,
            fatal: false,
            errors: 0,
        }
    }
}
impl Health {
    fn permitted(&self, now: Instant) -> bool {
        !self.fatal && self.retry_at.is_none_or(|t| now >= t)
    }
    fn ready(&self, now: Instant) -> bool {
        self.permitted(now)
            && self
                .verified_at
                .is_some_and(|t| now.saturating_duration_since(t) < VERIFY_INTERVAL)
    }
    fn error(&mut self, error: &Error, verification: bool, now: Instant) {
        // A permanent target refusal is parked by the queue, not an account
        // outage. Do not clear a concurrent account cooldown or revalidate bot.
        if matches!(error, Error::Rejected) && !verification {
            return;
        }
        self.errors = self.errors.saturating_add(1);
        let delay = Duration::from_secs((1u64 << self.errors.saturating_sub(1).min(5)).min(30));
        self.retry_at = now.checked_add(delay);
        match error {
            Error::Conflict => {
                self.phase = "duplicate_poller";
                self.fatal = true;
                self.verified_at = None;
            }
            Error::Credentials => {
                self.phase = "credentials_rejected";
                self.verified_at = None;
            }
            Error::Invalid | Error::Rejected if verification => {
                self.phase = "account_rejected";
                self.fatal = true;
                self.verified_at = None;
            }
            Error::Rejected => {
                self.phase = "target_rejected";
            }
            Error::RateLimited(seconds) => {
                self.phase = "rate_limited";
                self.retry_at =
                    now.checked_add(Duration::from_secs(u64::from((*seconds).clamp(1, 86400))));
            }
            _ => {
                self.phase = "transport_unavailable";
                self.verified_at = None;
            }
        }
    }
}
impl Account {
    #[cfg(test)]
    pub(crate) fn fixture_account_159(client: BotClient) -> Self {
        Self::new("bot_fixture".into(), 12345, Arc::new(client))
    }

    fn new(id: String, bot_id: i64, client: Arc<dyn AccountTransport>) -> Self {
        Self {
            id,
            bot_id,
            client,
            health: Mutex::new(Health::default()),
            polls: 0.into(),
            admitted: 0.into(),
            sent: 0.into(),
            failures: 0.into(),
        }
    }
    fn ready(&self) -> bool {
        self.health.lock().is_ok_and(|h| h.ready(Instant::now()))
    }
    fn protect(&self, operation: impl FnOnce()) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).is_err() {
            increment(&self.failures);
            if let Ok(mut h) = self.health.lock() {
                h.phase = "worker_stopped";
                h.fatal = true;
                h.verified_at = None;
            }
        }
    }
    fn error(&self, error: &Error, verification: bool) {
        increment(&self.failures);
        if let Ok(mut h) = self.health.lock() {
            h.error(error, verification, Instant::now());
        }
    }
}
/// Trusted operator files only. There is no browser-supplied filename and no
/// alternate Bot API origin. Reading/loading performs no network request.
#[derive(Debug, thiserror::Error)]
#[error("telegram_account_configuration_unavailable")]
pub struct ConfigurationError;
pub fn load(configs: &[AccountConfig]) -> Result<Vec<Account>, ConfigurationError> {
    if configs.len() > 20 {
        return Err(ConfigurationError);
    }
    let mut ids = HashSet::new();
    let mut bots = HashSet::new();
    let mut result = vec![];
    for c in configs {
        if !crate::store::valid_id(&c.account_id)
            || c.bot_id <= 0
            || !ids.insert(c.account_id.as_str())
            || !bots.insert(c.bot_id)
            || !c.token_file.is_absolute()
        {
            return Err(ConfigurationError);
        }
        let Some(bytes) = crate::private_store::read_config_optional(&c.token_file)
            .map_err(|_| ConfigurationError)?
        else {
            continue;
        };
        let secret: SecretFile = serde_json::from_slice(&bytes).map_err(|_| ConfigurationError)?;
        let client = BotClient::new(c.account_id.clone(), secret.token, c.bot_id)
            .map_err(|_| ConfigurationError)?;
        result.push(Account::new(
            c.account_id.clone(),
            c.bot_id,
            Arc::new(client),
        ));
    }
    Ok(result)
}
#[derive(Serialize)]
pub struct Status {
    pub account_id: String,
    pub bot_id: i64,
    pub state: &'static str,
    pub bot_identity_validated: bool,
    pub polls: u64,
    pub admitted_batches: u64,
    pub sent_chunks: u64,
    pub failures: u64,
}
pub struct TelegramWorkers {
    accounts: Vec<Arc<Account>>,
    stop: Arc<AtomicBool>,
    joins: Vec<JoinHandle<()>>,
}
impl TelegramWorkers {
    pub(crate) fn disabled(mut accounts: Vec<Account>) -> std::io::Result<Self> {
        if accounts.len() > 20 {
            return Err(std::io::Error::other("telegram_accounts_capacity"));
        }
        for account in &mut accounts {
            let health = account
                .health
                .get_mut()
                .map_err(|_| std::io::Error::other("telegram_state_unavailable"))?;
            health.phase = "operator_disabled";
            health.fatal = true;
        }
        Ok(Self {
            accounts: accounts.into_iter().map(Arc::new).collect(),
            stop: Arc::new(false.into()),
            joins: vec![],
        })
    }

    pub fn start<M: Mailbox + Directory + Send + 'static>(
        state: SharedHub<M>,
        accounts: Vec<Account>,
    ) -> std::io::Result<Self> {
        if accounts.len() > 20 {
            return Err(std::io::Error::other("telegram_accounts_capacity"));
        }
        let mut workers = Self {
            accounts: accounts.into_iter().map(Arc::new).collect(),
            stop: Arc::new(false.into()),
            joins: vec![],
        };
        for account in &workers.accounts {
            let a = account.clone();
            let s = state.clone();
            let stop = workers.stop.clone();
            workers.joins.push(
                thread::Builder::new()
                    .name("hub-telegram-poll".into())
                    .spawn(move || {
                        a.protect(|| {
                            while !stop.load(Ordering::Acquire) {
                                poll_once(&s, &a, &stop);
                                if a.health.lock().is_ok_and(|h| h.fatal) {
                                    break;
                                }
                                wait(&stop, Duration::from_millis(100));
                            }
                        });
                    })?,
            );
            let a = account.clone();
            let s = state.clone();
            let stop = workers.stop.clone();
            workers.joins.push(
                thread::Builder::new()
                    .name("hub-telegram-send".into())
                    .spawn(move || {
                        a.protect(|| {
                            while !stop.load(Ordering::Acquire) {
                                send_round(&s, &a, &stop);
                                if a.health.lock().is_ok_and(|h| h.fatal) {
                                    break;
                                }
                                wait(&stop, Duration::from_millis(100));
                            }
                        });
                    })?,
            );
        }
        Ok(workers)
    }
    pub(crate) fn readiness(&self) -> Arc<dyn Fn(&str) -> bool + Send + Sync> {
        let accounts = self.accounts.clone();
        let stop = self.stop.clone();
        Arc::new(move |id| {
            !stop.load(Ordering::Acquire)
                && accounts
                    .iter()
                    .find(|a| a.id == id)
                    .is_some_and(|a| a.ready())
        })
    }
    pub fn status(&self) -> Vec<Status> {
        self.accounts
            .iter()
            .map(|a| {
                let h = a.health.lock();
                let (phase, ready) = h
                    .as_ref()
                    .map(|h| (h.phase, h.ready(Instant::now())))
                    .unwrap_or(("state_unavailable", false));
                Status {
                    account_id: a.id.clone(),
                    bot_id: a.bot_id,
                    state: if self.stop.load(Ordering::Acquire) {
                        "stopped"
                    } else {
                        phase
                    },
                    bot_identity_validated: ready && !self.stop.load(Ordering::Acquire),
                    polls: a.polls.load(Ordering::Relaxed),
                    admitted_batches: a.admitted.load(Ordering::Relaxed),
                    sent_chunks: a.sent.load(Ordering::Relaxed),
                    failures: a.failures.load(Ordering::Relaxed),
                }
            })
            .collect()
    }
    /// Never join while holding the workers mutex: a snapshot holds hub first,
    /// while a draining worker may still need hub.
    pub(crate) fn stop_shared(shared: &Arc<Mutex<Self>>) -> Result<(), ()> {
        Self::stop_shared_with_timeout(shared, Duration::from_secs(12))
    }
    fn stop_shared_with_timeout(shared: &Arc<Mutex<Self>>, timeout: Duration) -> Result<(), ()> {
        let mut joins = {
            let mut workers = shared.lock().map_err(|_| ())?;
            workers.request_stop();
            std::mem::take(&mut workers.joins)
        };
        let began = Instant::now();
        while let Some(join) = joins.pop() {
            while !join.is_finished() && began.elapsed() < timeout {
                thread::sleep(Duration::from_millis(10));
            }
            if !join.is_finished() {
                joins.push(join);
                shared.lock().map_err(|_| ())?.joins.extend(joins);
                return Err(());
            }
            if join.join().is_err() {
                shared.lock().map_err(|_| ())?.joins.extend(joins);
                return Err(());
            }
        }
        Ok(())
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn stop(&mut self) {
        self.request_stop();
        for join in self.joins.drain(..) {
            let _ = join.join();
        }
    }
}
impl Drop for TelegramWorkers {
    fn drop(&mut self) {
        self.stop();
    }
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
fn wait(stop: &AtomicBool, d: Duration) {
    let began = Instant::now();
    while !stop.load(Ordering::Acquire) && began.elapsed() < d {
        thread::sleep(Duration::from_millis(25).min(d.saturating_sub(began.elapsed())));
    }
}
fn endpoints<M: Mailbox + Directory>(
    state: &SharedHub<M>,
    account: &str,
) -> Result<Vec<String>, ()> {
    let hub = state.lock().map_err(|_| ())?;
    let exists: bool = hub
        .store
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='telegram_targets')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| ())?;
    if !exists {
        return Ok(vec![]);
    }
    let mut query = hub
        .store
        .db
        .prepare(
            "SELECT endpoint FROM telegram_targets WHERE account=?1 ORDER BY endpoint LIMIT 21",
        )
        .map_err(|_| ())?;
    let ids = query
        .query_map([account], |r| r.get(0))
        .map_err(|_| ())?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|_| ())?;
    if ids.len() > 20 {
        return Err(());
    }
    Ok(ids)
}
fn pin_account<M: Mailbox + Directory>(state: &SharedHub<M>, account: &Account) -> Result<(), ()> {
    use rusqlite::OptionalExtension;
    let mut hub = state.lock().map_err(|_| ())?;
    let tx = hub.store.db.transaction().map_err(|_| ())?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS telegram_bot_pins(account TEXT PRIMARY KEY,bot_id INTEGER NOT NULL);").map_err(|_|())?;
    let old: Option<i64> = tx
        .query_row(
            "SELECT bot_id FROM telegram_bot_pins WHERE account=?1",
            [&account.id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ())?;
    match old {
        Some(id) if id != account.bot_id => return Err(()),
        Some(_) => {}
        None => {
            let count: i64 = tx
                .query_row("SELECT COUNT(*) FROM telegram_bot_pins", [], |r| r.get(0))
                .map_err(|_| ())?;
            if count >= 20 {
                return Err(());
            }
            tx.execute(
                "INSERT INTO telegram_bot_pins(account,bot_id) VALUES(?1,?2)",
                rusqlite::params![account.id, account.bot_id],
            )
            .map_err(|_| ())?;
        }
    }
    tx.commit().map_err(|_| ())
}
fn poll_once<M: Mailbox + Directory>(state: &SharedHub<M>, account: &Account, stop: &AtomicBool) {
    if stop.load(Ordering::Acquire) {
        return;
    }
    if !account
        .health
        .lock()
        .is_ok_and(|h| h.permitted(Instant::now()))
    {
        return;
    }
    if !account.ready() {
        if let Ok(mut h) = account.health.lock() {
            h.phase = "verifying";
            h.verified_at = None;
        }
        // No account/state lock survives this HTTP request.
        match account.client.verify() {
            Ok(()) => {
                if pin_account(state, account).is_err() {
                    increment(&account.failures);
                    if let Ok(mut h) = account.health.lock() {
                        h.phase = "account_pin_unavailable";
                        h.verified_at = None;
                        h.fatal = true;
                    }
                    return;
                }
                if let Ok(mut h) = account.health.lock() {
                    if h.fatal {
                        return;
                    }
                    h.phase = "ready";
                    h.verified_at = Some(Instant::now());
                    h.errors = 0;
                    h.retry_at = None;
                }
            }
            Err(error) => {
                account.error(&error, true);
                return;
            }
        }
    }
    if stop.load(Ordering::Acquire) {
        return;
    }
    let ids = match endpoints(state, &account.id) {
        Ok(ids) => ids,
        Err(()) => {
            increment(&account.failures);
            return;
        }
    };
    if ids.is_empty() {
        return;
    }
    let offset = match state
        .lock()
        .ok()
        .and_then(|hub| hub.telegram_offset(&account.id).ok())
    {
        Some(offset) => offset,
        None => {
            increment(&account.failures);
            return;
        }
    };
    if stop.load(Ordering::Acquire) || !account.ready() {
        return;
    }
    let updates = match account.client.poll(offset) {
        Ok(updates) => updates,
        Err(error) => {
            account.error(&error, false);
            return;
        }
    };
    increment(&account.polls);
    if stop.load(Ordering::Acquire) || !account.ready() {
        return;
    }
    let admitted = state
        .lock()
        .ok()
        .and_then(|mut hub| hub.admit_telegram(&account.id, updates, now().ok()?).ok())
        .is_some();
    if admitted {
        increment(&account.admitted);
        if let Ok(mut h) = account.health.lock() {
            if !h.fatal {
                h.phase = "ready";
                h.errors = 0;
            }
        }
    } else {
        increment(&account.failures);
        if let Ok(mut h) = account.health.lock() {
            h.phase = "backpressure";
            h.retry_at = Instant::now().checked_add(Duration::from_secs(1));
        }
    }
}
fn send_round<M: Mailbox + Directory>(state: &SharedHub<M>, account: &Account, stop: &AtomicBool) {
    if !account.ready() {
        return;
    }
    let ids = match endpoints(state, &account.id) {
        Ok(ids) => ids,
        Err(()) => {
            increment(&account.failures);
            return;
        }
    };
    for id in ids {
        if stop.load(Ordering::Acquire) || !account.ready() {
            return;
        }
        let mut bytes = [0; 32];
        if getrandom::fill(&mut bytes).is_err() {
            increment(&account.failures);
            return;
        }
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let (intent, revision) = {
            let Ok(mut hub) = state.lock() else {
                increment(&account.failures);
                return;
            };
            let Ok(time) = now() else {
                increment(&account.failures);
                return;
            };
            // Paused/unverified bindings are normal no-work, not Bot failures.
            let view = match hub.active(&id, time) {
                Ok(view) => view,
                Err(_) => continue,
            };
            match hub.prepare_telegram_chunk(&id, time, &token) {
                Ok(intent) => (intent, view.revision),
                Err(_) => {
                    increment(&account.failures);
                    continue;
                }
            }
        };
        let Some(intent) = intent else {
            continue;
        };
        if intent.target.account_id != account.id {
            increment(&account.failures);
            return;
        }
        if stop.load(Ordering::Acquire) || !account.ready() {
            return;
        }
        let allowed = state.lock().ok().is_some_and(|hub| {
            now()
                .ok()
                .and_then(|time| hub.active(&id, time).ok())
                .is_some_and(|view| view.revision == revision)
                && hub.blocked().is_ok_and(|blocked| !blocked)
        });
        if !allowed {
            continue;
        }
        if stop.load(Ordering::Acquire) || !account.ready() {
            return;
        }
        match account.client.send(&intent.target, &intent.text) {
            Ok(message) => {
                let committed = state
                    .lock()
                    .ok()
                    .and_then(|mut h| {
                        h.complete_telegram_chunk(&intent, message, now().ok()?)
                            .ok()
                    })
                    .is_some();
                if committed {
                    increment(&account.sent);
                } else {
                    increment(&account.failures);
                }
            }
            Err(error) => {
                if let Ok(mut hub) = state.lock() {
                    if let Ok(time) = now() {
                        let _ = hub.fail_telegram_chunk(&intent, &error, time);
                    }
                }
                account.error(&error, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{AgentMetadata, Delivery, Message, Principal},
        hub::{Binding, Hub, Readback},
        protocol::{Envelope, Sender, SenderKind},
        store::{Limits, Store},
        telegram::{Incoming, Target},
    };
    use std::{
        collections::VecDeque,
        fs,
        os::unix::fs::{symlink, PermissionsExt},
    };
    #[derive(Clone)]
    struct NoCore;
    impl Directory for NoCore {
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
    impl Mailbox for NoCore {
        fn receive(&self, _: u32) -> Result<Vec<Delivery>, crate::core::Error> {
            panic!("transport never owns Core receive")
        }
        fn ack(&self, _: &str) -> Result<(), crate::core::Error> {
            panic!("transport never owns Core ack")
        }
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<serde_json::Value>,
        ) -> Result<String, crate::core::Error> {
            panic!("transport never sends as an endpoint")
        }
    }
    #[derive(Default)]
    struct FakeBot {
        verified: AtomicBool,
        verifies: AtomicU64,
        polls: Mutex<VecDeque<Result<Vec<Update>, Error>>>,
        sent: Mutex<Vec<(String, i64, String)>>,
        rejected_chat: std::sync::atomic::AtomicI64,
    }
    impl AccountTransport for FakeBot {
        fn verify(&self) -> Result<(), Error> {
            increment(&self.verifies);
            self.verified.store(true, Ordering::Release);
            Ok(())
        }
        fn poll(&self, _: i64) -> Result<Vec<Update>, Error> {
            assert!(self.verified.load(Ordering::Acquire));
            self.polls.lock().unwrap().pop_front().unwrap_or(Ok(vec![]))
        }
    }
    impl Transport for FakeBot {
        fn send(&self, target: &Target, text: &str) -> Result<i64, Error> {
            assert!(self.verified.load(Ordering::Acquire));
            if target.chat_id == self.rejected_chat.load(Ordering::Relaxed) {
                return Err(Error::Rejected);
            }
            self.sent.lock().unwrap().push((
                target.account_id.clone(),
                target.chat_id,
                text.into(),
            ));
            Ok(42)
        }
    }
    fn fixture() -> SharedHub<NoCore> {
        fixture_with_outbound(true)
    }
    fn fixture_with_outbound(outbound: bool) -> SharedHub<NoCore> {
        let mut hub = Hub::new(
            Store::memory(Limits::default()).unwrap(),
            NoCore,
            "app_hub",
            "ws_test",
        )
        .unwrap();
        let time = now().unwrap();
        hub.add_binding_audited(
            Binding {
                endpoint_id: "ag_endpoint".into(),
                generation: 1,
                recipient: "ag_recipient".into(),
                expected_path: "/team/ag_endpoint".into(),
                expected_cwd: "/fixture".into(),
                expected_server_id: "machine_fixture".into(),
            },
            "user_owner",
            time,
        )
        .unwrap();
        hub.bind_telegram_audited(
            "ag_endpoint",
            1,
            Target {
                account_id: "bot_fixture".into(),
                chat_id: 100,
                topic_id: None,
                allowed_senders: vec![9],
            },
            "user_owner",
            time,
        )
        .unwrap();
        hub.resume_after_readback_audited(
            "ag_endpoint",
            2,
            Readback {
                observed: Some(NoCore.metadata("ag_endpoint").unwrap()),
                requested_at: time,
            },
            "user_owner",
            time,
        )
        .unwrap();
        if !outbound {
            return Arc::new(Mutex::new(hub));
        }
        let envelope = Envelope::Outbound {
            delivery_id: "delivery_original".into(),
            message_id: "msg_original".into(),
            original_sender: Sender {
                kind: SenderKind::Agent,
                id: "ag_recipient".into(),
            },
            body: "outbound fixture".into(),
            context_ids: vec![],
        };
        for (i, body) in envelope
            .fragments("ag_endpoint", 1)
            .unwrap()
            .into_iter()
            .enumerate()
        {
            hub.admit_core_delivery(
                &Delivery {
                    delivery_id: format!("delivery_{i}"),
                    recipient: Principal {
                        kind: SenderKind::Agent,
                        id: "app_hub".into(),
                    },
                    message: Message {
                        message_id: format!("msg_frame{i}"),
                        sender: Principal {
                            kind: SenderKind::Agent,
                            id: "ag_endpoint".into(),
                        },
                        body,
                        context_ids: vec![],
                    },
                },
                time,
            )
            .unwrap();
        }
        Arc::new(Mutex::new(hub))
    }
    #[test]
    fn disabled_operator_gate_does_not_start_transport_threads_or_verify_credentials() {
        let bot = Arc::new(FakeBot::default());
        let workers =
            TelegramWorkers::disabled(vec![Account::new("bot_fixture".into(), 12345, bot.clone())])
                .unwrap();
        assert!(workers.joins.is_empty());
        assert_eq!(workers.status()[0].state, "operator_disabled");
        assert!(!workers.readiness()("bot_fixture"));
        assert!(bot.sent.lock().unwrap().is_empty());
        assert_eq!(workers.accounts[0].polls.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn activation_readiness_is_live_expires_and_fails_closed_on_stop_or_cooldown() {
        let account = Arc::new(Account::new(
            "bot_fixture".into(),
            12345,
            Arc::new(FakeBot::default()),
        ));
        let mut workers = TelegramWorkers {
            accounts: vec![account.clone()],
            stop: Arc::new(false.into()),
            joins: vec![],
        };
        let ready = workers.readiness();
        assert!(!ready("bot_fixture"));
        assert!(!ready("unknown"));
        account.health.lock().unwrap().verified_at = Some(Instant::now());
        assert!(ready("bot_fixture"));
        account.health.lock().unwrap().retry_at = Some(Instant::now() + Duration::from_secs(30));
        assert!(!ready("bot_fixture"));
        account.health.lock().unwrap().retry_at = None;
        account.health.lock().unwrap().verified_at = Some(Instant::now() - VERIFY_INTERVAL);
        assert!(!ready("bot_fixture"));
        account.health.lock().unwrap().verified_at = Some(Instant::now());
        account.health.lock().unwrap().fatal = true;
        assert!(!ready("bot_fixture"));
        account.health.lock().unwrap().fatal = false;
        assert!(ready("bot_fixture"));
        workers.stop();
        assert!(!ready("bot_fixture"));
    }
    #[test]
    fn actual_poll_admission_and_chunk_loop_require_verification_and_persist_both_directions() {
        let state = fixture();
        let bot = Arc::new(FakeBot::default());
        let account = Account::new("bot_fixture".into(), 12345, bot.clone());
        let stop = AtomicBool::new(false);
        send_round(&state, &account, &stop);
        assert!(bot.sent.lock().unwrap().is_empty());
        bot.polls.lock().unwrap().push_back(Ok(vec![Update {
            update_id: 1,
            message: Some(Incoming {
                date: now().unwrap(),
                message_id: 1,
                chat_id: 100,
                topic_id: None,
                sender_id: 9,
                text: "inbound fixture".into(),
            }),
        }]));
        poll_once(&state, &account, &stop);
        assert!(account.ready());
        assert_eq!(
            state
                .lock()
                .unwrap()
                .telegram_offset("bot_fixture")
                .unwrap(),
            2
        );
        send_round(&state, &account, &stop);
        assert_eq!(account.sent.load(Ordering::Relaxed), 1);
        let sent = bot.sent.lock().unwrap();
        assert_eq!(sent[0].0, "bot_fixture");
        assert_eq!(sent[0].1, 100);
        assert!(sent[0].2.contains("outbound fixture"));
        drop(sent);
        // Persisted completion prevents an automatic duplicate send.
        send_round(&state, &account, &stop);
        assert_eq!(bot.sent.lock().unwrap().len(), 1);
        account.health.lock().unwrap().verified_at =
            Instant::now().checked_sub(VERIFY_INTERVAL + Duration::from_secs(1));
        poll_once(&state, &account, &stop);
        assert_eq!(bot.verifies.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn permanently_blocked_chat_parks_without_preventing_other_endpoint_send() {
        let state = fixture();
        let time = now().unwrap();
        {
            let mut hub = state.lock().unwrap();
            hub.add_binding_audited(
                Binding {
                    endpoint_id: "ag_second".into(),
                    generation: 1,
                    recipient: "ag_recipient".into(),
                    expected_path: "/team/ag_second".into(),
                    expected_cwd: "/fixture".into(),
                    expected_server_id: "machine_fixture".into(),
                },
                "user_owner",
                time,
            )
            .unwrap();
            hub.bind_telegram_audited(
                "ag_second",
                1,
                Target {
                    account_id: "bot_fixture".into(),
                    chat_id: 200,
                    topic_id: None,
                    allowed_senders: vec![9],
                },
                "user_owner",
                time,
            )
            .unwrap();
            hub.resume_after_readback_audited(
                "ag_second",
                2,
                Readback {
                    observed: Some(NoCore.metadata("ag_second").unwrap()),
                    requested_at: time,
                },
                "user_owner",
                time,
            )
            .unwrap();
            let envelope = Envelope::Outbound {
                delivery_id: "delivery_second".into(),
                message_id: "msg_second".into(),
                original_sender: Sender {
                    kind: SenderKind::Agent,
                    id: "ag_recipient".into(),
                },
                body: "second chat".into(),
                context_ids: vec![],
            };
            for (i, body) in envelope
                .fragments("ag_second", 1)
                .unwrap()
                .into_iter()
                .enumerate()
            {
                hub.admit_core_delivery(
                    &Delivery {
                        delivery_id: format!("delivery_second_{i}"),
                        recipient: Principal {
                            kind: SenderKind::Agent,
                            id: "app_hub".into(),
                        },
                        message: Message {
                            message_id: format!("msg_second_frame{i}"),
                            sender: Principal {
                                kind: SenderKind::Agent,
                                id: "ag_second".into(),
                            },
                            body,
                            context_ids: vec![],
                        },
                    },
                    time,
                )
                .unwrap();
            }
        }
        let bot = Arc::new(FakeBot::default());
        bot.rejected_chat.store(100, Ordering::Relaxed);
        let account = Account::new("bot_fixture".into(), 12345, bot.clone());
        let stop = AtomicBool::new(false);
        poll_once(&state, &account, &stop);
        send_round(&state, &account, &stop);
        assert!(account.ready());
        assert_eq!(bot.verifies.load(Ordering::Relaxed), 1);
        assert_eq!(account.sent.load(Ordering::Relaxed), 1);
        assert_eq!(bot.sent.lock().unwrap()[0].1, 200);
        // The next queue selection durably parks the permanent first result.
        send_round(&state, &account, &stop);
        assert_eq!(bot.verifies.load(Ordering::Relaxed), 1);
        assert_eq!(bot.sent.lock().unwrap().len(), 1);
        let hub = state.lock().unwrap();
        assert_eq!(hub.parked_transfers("ag_endpoint", 10).unwrap().len(), 1);
        assert!(hub.parked_transfers("ag_second", 10).unwrap().is_empty());
        assert!(!hub.view("ag_second").unwrap().paused);
    }
    #[test]
    fn duplicate_poller_is_sticky_and_rate_limit_or_credentials_block_new_sends() {
        let state = fixture();
        let bot = Arc::new(FakeBot::default());
        let account = Account::new("bot_fixture".into(), 12345, bot.clone());
        let stop = AtomicBool::new(false);
        bot.polls
            .lock()
            .unwrap()
            .push_back(Err(Error::RateLimited(600)));
        poll_once(&state, &account, &stop);
        assert!(!account.ready());
        send_round(&state, &account, &stop);
        assert!(bot.sent.lock().unwrap().is_empty());
        {
            let mut h = account.health.lock().unwrap();
            assert_eq!(h.phase, "rate_limited");
            h.retry_at = None;
        }
        bot.polls.lock().unwrap().push_back(Err(Error::Credentials));
        poll_once(&state, &account, &stop);
        assert!(!account.ready());
        assert_eq!(account.health.lock().unwrap().phase, "credentials_rejected");
        send_round(&state, &account, &stop);
        send_round(&state, &account, &stop);
        let attempts: i64 = state
            .lock()
            .unwrap()
            .store
            .db
            .query_row(
                "SELECT COALESCE(SUM(attempts),0) FROM telegram_chunks",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            attempts, 0,
            "unready account must not consume queue attempts"
        );
        account.health.lock().unwrap().retry_at = None;
        bot.polls.lock().unwrap().push_back(Err(Error::Conflict));
        poll_once(&state, &account, &stop);
        let before = bot.verifies.load(Ordering::Relaxed);
        account.health.lock().unwrap().retry_at = None;
        poll_once(&state, &account, &stop);
        assert_eq!(bot.verifies.load(Ordering::Relaxed), before);
        assert!(!account.ready());
        assert_eq!(account.health.lock().unwrap().phase, "duplicate_poller");
    }
    #[derive(Default)]
    struct BlockedBot {
        block_verification: bool,
        poll_calls: AtomicU64,
        inner: FakeBot,
        entered: AtomicBool,
        released: AtomicBool,
    }
    impl Transport for BlockedBot {
        fn send(&self, t: &Target, b: &str) -> Result<i64, Error> {
            self.inner.send(t, b)
        }
    }
    impl AccountTransport for BlockedBot {
        fn verify(&self) -> Result<(), Error> {
            if self.block_verification {
                self.entered.store(true, Ordering::Release);
                let deadline = Instant::now() + Duration::from_secs(3);
                while !self.released.load(Ordering::Acquire) {
                    assert!(
                        Instant::now() < deadline,
                        "verification fixture not released"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
            }
            self.inner.verify()
        }
        fn poll(&self, _: i64) -> Result<Vec<Update>, Error> {
            self.poll_calls.fetch_add(1, Ordering::Relaxed);
            assert!(
                !self.block_verification,
                "stopped verification must not poll"
            );
            self.entered.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(3);
            while !self.released.load(Ordering::Acquire) {
                if Instant::now() >= deadline {
                    return Err(Error::Transport);
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(vec![])
        }
    }
    #[test]
    fn an_owned_slow_account_does_not_hold_state_or_stop_another_accounts_send_loop() {
        let state = fixture();
        let time = now().unwrap();
        {
            let mut hub = state.lock().unwrap();
            hub.add_binding_audited(
                Binding {
                    endpoint_id: "ag_blocked".into(),
                    generation: 1,
                    recipient: "ag_recipient".into(),
                    expected_path: "/team/ag_blocked".into(),
                    expected_cwd: "/fixture".into(),
                    expected_server_id: "machine_fixture".into(),
                },
                "user_owner",
                time,
            )
            .unwrap();
            hub.bind_telegram_audited(
                "ag_blocked",
                1,
                Target {
                    account_id: "bot_blocked".into(),
                    chat_id: 200,
                    topic_id: None,
                    allowed_senders: vec![9],
                },
                "user_owner",
                time,
            )
            .unwrap();
            hub.resume_after_readback_audited(
                "ag_blocked",
                2,
                Readback {
                    observed: Some(NoCore.metadata("ag_blocked").unwrap()),
                    requested_at: time,
                },
                "user_owner",
                time,
            )
            .unwrap();
        }
        let slow = Arc::new(BlockedBot::default());
        let fast = Arc::new(FakeBot::default());
        let mut workers = TelegramWorkers::start(
            state.clone(),
            vec![
                Account::new("bot_blocked".into(), 111, slow.clone()),
                Account::new("bot_fixture".into(), 12345, fast.clone()),
            ],
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !slow.entered.load(Ordering::Acquire) || fast.sent.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while state.try_lock().is_err() {
            assert!(Instant::now() < deadline, "HTTP wait held actor");
            thread::sleep(Duration::from_millis(2));
        }
        slow.released.store(true, Ordering::Release);
        workers.stop();
        let statuses = workers.status();
        assert_eq!(statuses.len(), 2);
        assert!(statuses
            .iter()
            .all(|s| s.state == "stopped" && !s.bot_identity_validated));
        let serialized = serde_json::to_string(&statuses).unwrap();
        assert!(!serialized.contains("token"));
        assert!(!serialized.contains("token_file"));
    }
    #[test]
    fn a_configuration_change_cannot_reassign_an_existing_account_to_a_different_bot() {
        let state = fixture();
        let bot = Arc::new(FakeBot::default());
        let first = Account::new("bot_fixture".into(), 12345, bot.clone());
        let stop = AtomicBool::new(false);
        poll_once(&state, &first, &stop);
        assert!(first.ready());
        let changed = Account::new("bot_fixture".into(), 54321, Arc::new(FakeBot::default()));
        poll_once(&state, &changed, &stop);
        assert!(!changed.ready());
        assert_eq!(
            changed.health.lock().unwrap().phase,
            "account_pin_unavailable"
        );
        assert_eq!(
            state
                .lock()
                .unwrap()
                .store
                .db
                .query_row(
                    "SELECT bot_id FROM telegram_bot_pins WHERE account='bot_fixture'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            12345
        );
    }
    #[test]
    fn token_change_drain_times_out_safely_and_mid_poll_does_not_admit() {
        // Isolate poll-drain admission: no independent outbound work can finish
        // before stop is requested and contaminate the lifetime send counter.
        let state = fixture_with_outbound(false);
        let slow = Arc::new(BlockedBot::default());
        let workers = Arc::new(Mutex::new(
            TelegramWorkers::start(
                state.clone(),
                vec![Account::new("bot_fixture".into(), 12345, slow.clone())],
            )
            .unwrap(),
        ));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !slow.entered.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            TelegramWorkers::stop_shared_with_timeout(&workers, Duration::from_millis(10)).is_err()
        );
        assert!(!workers.lock().unwrap().readiness()("bot_fixture"));
        assert_eq!(workers.lock().unwrap().status()[0].state, "stopped");
        slow.released.store(true, Ordering::Release);
        TelegramWorkers::stop_shared(&workers).unwrap();
        assert_eq!(workers.lock().unwrap().status()[0].admitted_batches, 0);
        assert_eq!(workers.lock().unwrap().status()[0].sent_chunks, 0);
    }
    #[test]
    fn drain_before_verification_preserves_unsent_pending_outbound_work() {
        let state = fixture();
        let slow = Arc::new(BlockedBot {
            block_verification: true,
            ..BlockedBot::default()
        });
        let pending = || {
            state
                .lock()
                .unwrap()
                .store
                .db
                .query_row(
                    "SELECT count(*) FROM transfers WHERE endpoint='ag_endpoint' AND state=1",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
        };
        assert_eq!(pending(), 1);
        let workers = Arc::new(Mutex::new(
            TelegramWorkers::start(
                state.clone(),
                vec![Account::new("bot_fixture".into(), 12345, slow.clone())],
            )
            .unwrap(),
        ));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !slow.entered.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        // Verification is blocked: the queued message demonstrably exists and
        // is unsent when stop is requested, independent of sender scheduling.
        assert!(slow.inner.sent.lock().unwrap().is_empty());
        assert_eq!(pending(), 1);
        assert!(
            TelegramWorkers::stop_shared_with_timeout(&workers, Duration::from_millis(10)).is_err()
        );
        assert!(!workers.lock().unwrap().readiness()("bot_fixture"));
        slow.released.store(true, Ordering::Release);
        TelegramWorkers::stop_shared(&workers).unwrap();
        // Joining both workers is the boundary; no transport thread survives it.
        let status = workers.lock().unwrap().status();
        // Account::protect catches mock panics; assert invocation and failure
        // counts in the parent so a protected panic cannot hide a bad path.
        assert_eq!(slow.poll_calls.load(Ordering::Relaxed), 0);
        assert_eq!(status[0].failures, 0);
        assert_eq!(status[0].sent_chunks, 0);
        assert_eq!(status[0].admitted_batches, 0);
        assert!(slow.inner.sent.lock().unwrap().is_empty());
        assert_eq!(pending(), 1);
    }
    #[test]
    fn drain_preserves_send_completed_before_stop_without_admitting_blocked_poll() {
        let state = fixture();
        let slow = Arc::new(BlockedBot::default());
        let workers = Arc::new(Mutex::new(
            TelegramWorkers::start(
                state,
                vec![Account::new("bot_fixture".into(), 12345, slow.clone())],
            )
            .unwrap(),
        ));
        let deadline = Instant::now() + Duration::from_secs(2);
        // A blocked poll has already verified the account. The independent
        // sender may therefore finish queued work before any stop request.
        while !slow.entered.load(Ordering::Acquire)
            || workers.lock().unwrap().status()[0].sent_chunks != 1
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(slow.inner.sent.lock().unwrap().len(), 1);
        assert!(
            TelegramWorkers::stop_shared_with_timeout(&workers, Duration::from_millis(10)).is_err()
        );
        assert!(!workers.lock().unwrap().readiness()("bot_fixture"));
        slow.released.store(true, Ordering::Release);
        TelegramWorkers::stop_shared(&workers).unwrap();
        let status = workers.lock().unwrap().status();
        assert_eq!(status[0].admitted_batches, 0);
        assert_eq!(status[0].sent_chunks, 1);
        assert_eq!(slow.inner.sent.lock().unwrap().len(), 1);
    }
    #[test]
    fn secret_startup_files_are_private_strict_and_never_have_an_origin_override() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let file = root.join("bot.json");
        fs::write(&file, r#"{"token":"12345:synthetic_abcdefghijklmnopqrst"}"#).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let c = AccountConfig {
            account_id: "bot_fixture".into(),
            bot_id: 12345,
            token_file: file.clone(),
        };
        assert_eq!(load(&[c]).unwrap().len(), 1);
        let link = root.join("link.json");
        symlink(&file, &link).unwrap();
        assert!(load(&[AccountConfig {
            account_id: "bot_fixture".into(),
            bot_id: 12345,
            token_file: link
        }])
        .is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&[AccountConfig {
            account_id: "bot_fixture".into(),
            bot_id: 12345,
            token_file: file
        }])
        .is_err());
        assert!(serde_json::from_value::<AccountConfig>(serde_json::json!({"account_id":"bot_fixture","bot_id":12345,"token_file":"/private/bot.json","api_origin":"http://127.0.0.1:1"})).is_err());
    }
}
