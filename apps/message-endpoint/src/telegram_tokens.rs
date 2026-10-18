//! Write-only owner credential provisioning. Credentials never enter SQLite,
//! settings responses, audit detail, Debug, logs or client-selected paths.
//! Changing any token drains all Telegram workers; only an operator restart can
//! load credentials again, and binding resume remains a separate owner action.
use crate::{
    hub::Directory,
    owner_auth::Owner,
    relay::Mailbox,
    scheduler::SharedHub,
    telegram_workers::{AccountConfig, TelegramWorkers},
};
use rusqlite::params;
use rustix::fs::{self, AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Set {
        account_id: String,
        token: String,
        confirm: bool,
    },
    Remove {
        account_id: String,
        confirm: bool,
    },
}
impl Command {
    pub fn removes(&self) -> bool {
        matches!(self, Self::Remove { .. })
    }
}
#[derive(Clone, Default, Serialize)]
pub struct Slot {
    pub account_id: String,
    pub bot_id: i64,
    pub configured: bool,
    pub state: &'static str,
    pub last4: Option<String>,
    pub restart_required: bool,
}
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum Failure {
    #[error("telegram_token_invalid")]
    Invalid,
    #[error("telegram_token_different_bot")]
    DifferentBot,
    #[error("telegram_token_rejected")]
    Rejected,
    #[error("telegram_token_verification_unavailable")]
    Verification,
    #[error("telegram_tokens_pause_all_endpoints_first")]
    Active,
    #[error("telegram_token_storage_unavailable")]
    Storage,
    #[error("owner_identity_invalid")]
    Expired,
}
pub type Verify = dyn Fn(&str, &str, i64) -> Result<(), Failure> + Send + Sync;
pub type Mutation = dyn Fn(&Owner, Command) -> Result<Slot, Failure> + Send + Sync;

pub fn valid_shape(token: &str) -> Option<i64> {
    let (id, secret) = token.split_once(':')?;
    if !(3..=20).contains(&id.len())
        || !id.bytes().all(|b| b.is_ascii_digit())
        || secret.len() != 35
        || !secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return None;
    }
    id.parse::<i64>().ok().filter(|id| *id > 0)
}
fn clock(owner: &Owner) -> Result<i64, Failure> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|t| i64::try_from(t.as_secs()).ok())
        .ok_or(Failure::Storage)?;
    if now >= owner.expires_at() {
        return Err(Failure::Expired);
    }
    Ok(now)
}
fn storage<T, E>(r: Result<T, E>) -> Result<T, Failure> {
    r.map_err(|_| Failure::Storage)
}
/// Paths come only from startup configuration. All ancestors use descriptor-
/// relative no-follow traversal. A private directory does not isolate same-UID code.
fn change_file(path: &Path, token: Option<&str>) -> Result<(), Failure> {
    let directory = storage(crate::private_store::open_directory(
        path.parent().ok_or(Failure::Storage)?,
        false,
    ))?;
    let name = path.file_name().ok_or(Failure::Storage)?;
    let check = || -> Result<bool, Failure> {
        match fs::openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::empty(),
        ) {
            Ok(file) => {
                storage(crate::private_store::check_file(&file))?;
                Ok(true)
            }
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(_) => Err(Failure::Storage),
        }
    };
    let exists = check()?;
    if let Some(token) = token {
        let mut random = [0u8; 32];
        storage(getrandom::fill(&mut random))?;
        let temporary = format!(
            ".telegram-token-{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let fd = storage(fs::openat(
            &directory,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ))?;
        let result = (|| {
            let mut file = std::fs::File::from(fd);
            let bytes = storage(serde_json::to_vec(&serde_json::json!({"token":token})))?;
            storage(file.write_all(&bytes))?;
            storage(file.sync_all())?;
            check()?;
            storage(fs::renameat(
                &directory,
                temporary.as_str(),
                &directory,
                name,
            ))?;
            storage(fs::fsync(&directory))
        })();
        let _ = fs::unlinkat(&directory, temporary.as_str(), AtFlags::empty());
        result
    } else {
        if exists {
            storage(fs::unlinkat(&directory, name, AtFlags::empty()))?;
        }
        storage(fs::fsync(&directory))
    }
}

pub struct Service {
    configs: Vec<AccountConfig>,
    slots: Mutex<Vec<Slot>>,
    serial: Mutex<()>,
    verify: Arc<Verify>,
}
impl Service {
    pub fn new(configs: Vec<AccountConfig>) -> Result<Self, Failure> {
        Self::with_verifier(
            configs,
            Arc::new(|account, token, bot| {
                let client =
                    crate::telegram_http::BotClient::new(account.into(), token.into(), bot)
                        .map_err(|_| Failure::Invalid)?;
                client.verify().map_err(|e| match e {
                    crate::telegram::Error::Credentials
                    | crate::telegram::Error::Rejected
                    | crate::telegram::Error::Invalid => Failure::Rejected,
                    _ => Failure::Verification,
                })
            }),
        )
    }
    fn with_verifier(configs: Vec<AccountConfig>, verify: Arc<Verify>) -> Result<Self, Failure> {
        if configs.len() > 20 {
            return Err(Failure::Invalid);
        }
        let mut ids = HashSet::new();
        let mut bots = HashSet::new();
        let mut paths = HashSet::new();
        let mut slots = vec![];
        for c in &configs {
            if !crate::store::valid_id(&c.account_id)
                || c.bot_id <= 0
                || !ids.insert(c.account_id.clone())
                || !bots.insert(c.bot_id)
                || !paths.insert(c.token_file.clone())
            {
                return Err(Failure::Invalid);
            }
            let bytes = storage(crate::private_store::read_config_optional(&c.token_file))?;
            let last4 = match bytes {
                Some(bytes) => {
                    #[derive(Deserialize)]
                    #[serde(deny_unknown_fields)]
                    struct Secret {
                        token: String,
                    }
                    let value: Secret = storage(serde_json::from_slice(&bytes))?;
                    // Existing provisioned tokens retain the loader's compatibility.
                    if value.token.len() < 4 {
                        return Err(Failure::Storage);
                    }
                    Some(
                        value
                            .token
                            .chars()
                            .rev()
                            .take(4)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect(),
                    )
                }
                None => None,
            };
            slots.push(Slot {
                account_id: c.account_id.clone(),
                bot_id: c.bot_id,
                configured: last4.is_some(),
                state: if last4.is_some() {
                    "token_stored"
                } else {
                    "token_missing"
                },
                last4,
                restart_required: false,
            });
        }
        Ok(Self {
            configs,
            slots: Mutex::new(slots),
            serial: Mutex::new(()),
            verify,
        })
    }
    pub fn snapshot(&self) -> Result<Vec<Slot>, Failure> {
        Ok(storage(self.slots.lock())?.clone())
    }
    pub fn execute<M: Mailbox + Directory>(
        &self,
        shared: &SharedHub<M>,
        workers: &Arc<Mutex<TelegramWorkers>>,
        owner: &Owner,
        command: Command,
    ) -> Result<Slot, Failure> {
        let _serial = storage(self.serial.lock())?;
        clock(owner)?;
        let removes = command.removes();
        let (account, token, confirmed) = match &command {
            Command::Set {
                account_id,
                token,
                confirm,
            } => (account_id, Some(token.as_str()), *confirm),
            Command::Remove {
                account_id,
                confirm,
            } => (account_id, None, *confirm),
        };
        if !confirmed {
            return Err(Failure::Invalid);
        }
        let config = self
            .configs
            .iter()
            .find(|c| &c.account_id == account)
            .ok_or(Failure::Invalid)?;
        if let Some(token) = token {
            if valid_shape(token).ok_or(Failure::Invalid)? != config.bot_id {
                return Err(Failure::DifferentBot);
            }
            // Cheap refusal before exposing a candidate credential to Telegram.
            // The same pause condition is checked again after network I/O.
            {
                let hub = storage(shared.lock())?;
                for id in storage(hub.binding_ids())? {
                    if !storage(hub.view(&id))?.paused {
                        return Err(Failure::Active);
                    }
                }
            }
            (self.verify)(account, token, config.bot_id)?;
        }
        {
            let mut hub = storage(shared.lock())?;
            let now = clock(owner)?;
            for id in storage(hub.binding_ids())? {
                let view = storage(hub.view(&id))?;
                if !view.paused {
                    if !removes {
                        return Err(Failure::Active);
                    }
                    storage(hub.pause_audited(&id, view.revision, owner.id(), now))?;
                }
            }
            // Under the same hub lock as resume's final readiness check.
            storage(workers.lock())?.request_stop();
            for slot in storage(self.slots.lock())?.iter_mut() {
                slot.restart_required = true;
            }
        }
        storage(TelegramWorkers::stop_shared(workers))?;
        let mut hub = storage(shared.lock())?;
        let now = clock(owner)?;
        storage(hub.store.recheck_private_storage())?;
        storage(hub.recovery_schema())?;
        storage(hub.store.db.execute_batch("CREATE TABLE IF NOT EXISTS telegram_token_containment(account TEXT PRIMARY KEY,actor TEXT NOT NULL,bot_id INTEGER NOT NULL,at INTEGER NOT NULL,count INTEGER NOT NULL);"))?;
        storage(hub.store.db.execute_batch("CREATE TABLE IF NOT EXISTS telegram_bot_pins(account TEXT PRIMARY KEY,bot_id INTEGER NOT NULL);"))?;
        let last4 = token.map(|t| t[t.len() - 4..].to_owned());
        let detail = serde_json::json!({"bot_id":config.bot_id,"last4":last4}).to_string();
        let tx = storage(hub.store.db.transaction())?;
        let context = storage(crate::owner_actions::AuditContext::with_detail(
            owner.id(),
            now,
            &detail,
        ))?;
        let full: bool = storage(tx.query_row(
            "SELECT COUNT(*)>=?1 FROM hub_owner_audit",
            [crate::recovery::AUDIT_LIMIT],
            |r| r.get(0),
        ))?;
        if full && !removes {
            return Err(Failure::Storage);
        }
        if !full {
            storage(crate::owner_actions::record(
                &tx,
                Some(&context),
                if removes {
                    "telegram_token_remove"
                } else {
                    "telegram_token_set"
                },
                account,
                0,
            ))?;
        }
        if removes {
            storage(tx.execute("INSERT INTO telegram_token_containment(account,actor,bot_id,at,count) VALUES(?1,?2,?3,?4,1) ON CONFLICT(account) DO UPDATE SET actor=excluded.actor,bot_id=excluded.bot_id,at=excluded.at,count=CASE WHEN count<9223372036854775807 THEN count+1 ELSE count END",params![account,owner.id(),config.bot_id,now]))?;
        }
        if !removes {
            let prior: Option<i64> = {
                use rusqlite::OptionalExtension;
                storage(
                    tx.query_row(
                        "SELECT bot_id FROM telegram_bot_pins WHERE account=?1",
                        [account],
                        |r| r.get(0),
                    )
                    .optional(),
                )?
            };
            if prior.is_some_and(|bot| bot != config.bot_id) {
                return Err(Failure::DifferentBot);
            }
            storage(tx.execute(
                "INSERT OR IGNORE INTO telegram_bot_pins(account,bot_id) VALUES(?1,?2)",
                params![account, config.bot_id],
            ))?;
        }
        // Audit describes the confirmed attempt, committed before filesystem I/O.
        // A filesystem failure leaves transport stopped; no success is reported.
        storage(tx.commit())?;
        if change_file(&config.token_file, token).is_err() {
            if let Some(slot) = storage(self.slots.lock())?
                .iter_mut()
                .find(|s| &s.account_id == account)
            {
                slot.state = "storage_unavailable";
                slot.last4 = None;
            }
            return Err(Failure::Storage);
        }
        let mut slots = storage(self.slots.lock())?;
        let slot = slots
            .iter_mut()
            .find(|s| &s.account_id == account)
            .ok_or(Failure::Invalid)?;
        slot.configured = token.is_some();
        slot.state = if token.is_some() {
            "token_stored"
        } else {
            "token_missing"
        };
        slot.last4 = last4;
        slot.restart_required = true;
        Ok(slot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{AgentMetadata, Delivery},
        hub::Hub,
        store::{Limits, Store},
    };
    use std::{
        fs as stdfs,
        os::unix::fs::{symlink, PermissionsExt},
        sync::atomic::{AtomicUsize, Ordering},
    };
    const TOKEN: &str = "12345:ABCDEFGHIJKLMNOPQRSTUVWXYZ_23456789";
    #[derive(Clone)]
    struct NoCore;
    impl Directory for NoCore {
        fn metadata(&self, _: &str) -> Result<AgentMetadata, crate::core::Error> {
            Err(crate::core::Error::Unavailable)
        }
    }
    impl Mailbox for NoCore {
        fn receive(&self, _: u32) -> Result<Vec<Delivery>, crate::core::Error> {
            panic!("no Core calls")
        }
        fn ack(&self, _: &str) -> Result<(), crate::core::Error> {
            panic!("no Core calls")
        }
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<serde_json::Value>,
        ) -> Result<String, crate::core::Error> {
            panic!("no Core calls")
        }
    }
    fn root() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        stdfs::set_permissions(d.path(), stdfs::Permissions::from_mode(0o700)).unwrap();
        d
    }
    fn config(root: &Path) -> AccountConfig {
        AccountConfig {
            account_id: "bot_fixture".into(),
            bot_id: 12345,
            token_file: stdfs::canonicalize(root).unwrap().join("token.json"),
        }
    }
    fn state(root: &Path) -> SharedHub<NoCore> {
        Arc::new(Mutex::new(
            Hub::new(
                Store::open(
                    &stdfs::canonicalize(root).unwrap().join("state"),
                    Limits::default(),
                )
                .unwrap(),
                NoCore,
                "app_hub",
                "ws_test",
            )
            .unwrap(),
        ))
    }
    fn owner() -> Owner {
        Owner::synthetic("user_owner", i64::MAX)
    }
    fn set(token: &str) -> Command {
        Command::Set {
            account_id: "bot_fixture".into(),
            token: token.into(),
            confirm: true,
        }
    }
    fn remove() -> Command {
        Command::Remove {
            account_id: "bot_fixture".into(),
            confirm: true,
        }
    }
    fn workers() -> Arc<Mutex<TelegramWorkers>> {
        Arc::new(Mutex::new(TelegramWorkers::disabled(vec![]).unwrap()))
    }
    #[test]
    fn strict_shape_rejects_controls_wrong_length_and_non_ascii() {
        assert_eq!(valid_shape(TOKEN), Some(12345));
        for bad in [
            format!(" {TOKEN}"),
            format!("{TOKEN}\n"),
            TOKEN.replace('_', ":"),
            TOKEN.replace('_', "é"),
            TOKEN[..TOKEN.len() - 1].into(),
            format!("{TOKEN}a"),
            "0:ABCDEFGHIJKLMNOPQRSTUVWXYZ_23456789".into(),
        ] {
            assert!(valid_shape(&bad).is_none());
        }
    }
    #[test]
    fn missing_slot_store_replace_remove_and_no_secret_persistence() {
        let r = root();
        let c = config(r.path());
        let path = c.token_file.clone();
        assert!(crate::telegram_workers::load(std::slice::from_ref(&c))
            .unwrap()
            .is_empty());
        let service = Service::with_verifier(vec![c], Arc::new(|_, _, _| Ok(()))).unwrap();
        let shared = state(r.path());
        let workers = workers();
        assert!(!service.snapshot().unwrap()[0].configured);
        let slot = service
            .execute(&shared, &workers, &owner(), set(TOKEN))
            .unwrap();
        assert!(slot.configured && slot.restart_required);
        assert_eq!(slot.last4.as_deref(), Some("6789"));
        assert_eq!(
            stdfs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&stdfs::read(&path).unwrap()).unwrap()
                ["token"],
            TOKEN
        );
        let replacement = TOKEN.replace("6789", "9876");
        service
            .execute(&shared, &workers, &owner(), set(&replacement))
            .unwrap();
        assert!(stdfs::read_to_string(&path).unwrap().contains(&replacement));
        let public = serde_json::to_string(&service.snapshot().unwrap()).unwrap();
        assert!(
            !public.contains(TOKEN)
                && !public.contains(&replacement)
                && !public.contains("token_file")
        );
        {
            let h = shared.lock().unwrap();
            h.store.db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) SELECT 'user_owner','fixture','ag_fixture','',0,0 FROM n;").unwrap();
        }
        assert!(
            !service
                .execute(&shared, &workers, &owner(), remove())
                .unwrap()
                .configured
        );
        assert!(!path.exists());
        assert!(!workers.lock().unwrap().readiness()("bot_fixture"));
        for entry in stdfs::read_dir(stdfs::canonicalize(r.path()).unwrap().join("state")).unwrap()
        {
            let bytes = stdfs::read(entry.unwrap().path()).unwrap();
            for secret in [TOKEN, &replacement] {
                assert!(
                    !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
                    "secret reached SQLite or sidecar"
                );
            }
        }
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .store
                .db
                .query_row("SELECT COUNT(*) FROM telegram_token_containment", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn invalid_different_bot_expired_and_failed_getme_keep_old_file() {
        let r = root();
        let c = config(r.path());
        change_file(&c.token_file, Some(TOKEN)).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let service = Service::with_verifier(
            vec![c.clone()],
            Arc::new(move |_, _, _| {
                counted.fetch_add(1, Ordering::Relaxed);
                Err(Failure::Verification)
            }),
        )
        .unwrap();
        let shared = state(r.path());
        let workers = workers();
        assert!(matches!(
            service.execute(&shared, &workers, &owner(), set("bad")),
            Err(Failure::Invalid)
        ));
        assert!(matches!(
            service.execute(
                &shared,
                &workers,
                &owner(),
                set(&TOKEN.replace("12345:", "54321:"))
            ),
            Err(Failure::DifferentBot)
        ));
        assert!(matches!(
            service.execute(
                &shared,
                &workers,
                &Owner::synthetic("user_owner", 1),
                set(TOKEN)
            ),
            Err(Failure::Expired)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(matches!(
            service.execute(&shared, &workers, &owner(), set(TOKEN)),
            Err(Failure::Verification)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(stdfs::read_to_string(c.token_file).unwrap().contains(TOKEN));
        assert!(!format!("{:?}", Failure::Verification).contains(TOKEN));
    }
    #[test]
    fn actual_getme_success_failure_wrong_identity_never_persist_or_echo_secret() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        for (status, id, accepted) in [(200, 12345, true), (200, 54321, false), (503, 12345, false)]
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!(
                "http://{}/TREER_TEST_ONLY_BOT_ORIGIN_159",
                listener.local_addr().unwrap()
            );
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0; 4096];
                let count = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..count]).contains("/getMe"));
                let body=serde_json::json!({"ok":true,"result":{"id":id,"is_bot":true},"description":TOKEN}).to_string();
                write!(stream,"HTTP/1.1 {status} Synthetic\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            let r = root();
            let c = config(r.path());
            let path = c.token_file.clone();
            let service = Service::with_verifier(
                vec![c],
                Arc::new(move |account, token, bot| {
                    let client = crate::telegram_http::BotClient::with_test_origin_159(
                        account.into(),
                        token.into(),
                        bot,
                        origin.clone(),
                    );
                    client.verify().map_err(|_| Failure::Verification)
                }),
            )
            .unwrap();
            let shared = state(r.path());
            let result = service.execute(&shared, &workers(), &owner(), set(TOKEN));
            assert_eq!(result.is_ok(), accepted);
            assert_eq!(path.exists(), accepted);
            if let Err(error) = result {
                assert!(!error.to_string().contains(TOKEN));
            }
            let public = serde_json::to_string(&service.snapshot().unwrap()).unwrap();
            assert!(!public.contains(TOKEN));
            for entry in
                stdfs::read_dir(stdfs::canonicalize(r.path()).unwrap().join("state")).unwrap()
            {
                let bytes = stdfs::read(entry.unwrap().path()).unwrap();
                assert!(!bytes.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()));
            }
            server.join().unwrap();
        }
    }
    #[test]
    fn active_bindings_block_set_but_remove_pauses_and_pins_cannot_change_bot() {
        let r = root();
        let c = config(r.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let service = Service::with_verifier(
            vec![c],
            Arc::new(move |_, _, _| {
                counted.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }),
        )
        .unwrap();
        let shared = state(r.path());
        let workers = workers();
        {
            let mut h = shared.lock().unwrap();
            h.add_binding_audited(
                crate::hub::Binding {
                    endpoint_id: "ag_endpoint".into(),
                    generation: 1,
                    recipient: "ag_recipient".into(),
                    expected_path: "/sandbox/endpoint".into(),
                    expected_cwd: "/fixture".into(),
                    expected_server_id: "machine_fixture".into(),
                },
                "user_owner",
                1,
            )
            .unwrap();
            h.store
                .db
                .execute("UPDATE endpoints SET paused=0", [])
                .unwrap();
        }
        assert!(matches!(
            service.execute(&shared, &workers, &owner(), set(TOKEN)),
            Err(Failure::Active)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        service
            .execute(&shared, &workers, &owner(), remove())
            .unwrap();
        assert!(shared.lock().unwrap().view("ag_endpoint").unwrap().paused);
        service
            .execute(&shared, &workers, &owner(), set(TOKEN))
            .unwrap();
        shared
            .lock()
            .unwrap()
            .store
            .db
            .execute("UPDATE telegram_bot_pins SET bot_id=54321", [])
            .unwrap();
        assert!(matches!(
            service.execute(&shared, &workers, &owner(), set(TOKEN)),
            Err(Failure::DifferentBot)
        ));
    }
    #[test]
    fn file_failure_reports_stopped_state_and_expiry_is_rechecked_after_getme() {
        let r = root();
        let c = config(r.path());
        let path = c.token_file.clone();
        let service = Service::with_verifier(vec![c.clone()], Arc::new(|_, _, _| Ok(()))).unwrap();
        let outside = stdfs::canonicalize(r.path()).unwrap().join("outside");
        change_file(&outside, Some(TOKEN)).unwrap();
        symlink(&outside, &path).unwrap();
        let shared = state(r.path());
        let workers = workers();
        assert!(matches!(
            service.execute(&shared, &workers, &owner(), set(TOKEN)),
            Err(Failure::Storage)
        ));
        let slot = service.snapshot().unwrap().remove(0);
        assert!(slot.restart_required);
        assert_eq!(slot.state, "storage_unavailable");
        assert!(!workers.lock().unwrap().readiness()("bot_fixture"));
        assert!(stdfs::read_to_string(&outside).unwrap().contains(TOKEN));
        stdfs::remove_file(&path).unwrap();
        let delayed = Service::with_verifier(
            vec![c],
            Arc::new(|_, _, _| {
                std::thread::sleep(std::time::Duration::from_millis(1100));
                Ok(())
            }),
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(matches!(
            delayed.execute(
                &shared,
                &workers,
                &Owner::synthetic("user_owner", now + 1),
                set(TOKEN)
            ),
            Err(Failure::Expired)
        ));
        assert!(!path.exists());
    }
    #[test]
    fn symlinks_hardlinks_and_bad_directories_refused_without_touching_target() {
        let r = root();
        let c = config(r.path());
        let outside = stdfs::canonicalize(r.path()).unwrap().join("outside");
        change_file(&outside, Some(TOKEN)).unwrap();
        symlink(&outside, &c.token_file).unwrap();
        for token in [Some(TOKEN), None] {
            assert!(change_file(&c.token_file, token).is_err());
        }
        assert!(Service::new(vec![c.clone()]).is_err());
        stdfs::remove_file(&c.token_file).unwrap();
        stdfs::hard_link(&outside, &c.token_file).unwrap();
        for token in [Some(TOKEN), None] {
            assert!(change_file(&c.token_file, token).is_err());
        }
        assert!(stdfs::read_to_string(&outside).unwrap().contains(TOKEN));
        stdfs::remove_file(&c.token_file).unwrap();
        stdfs::set_permissions(r.path(), stdfs::Permissions::from_mode(0o755)).unwrap();
        assert!(change_file(&c.token_file, Some(TOKEN)).is_err());
    }
}
