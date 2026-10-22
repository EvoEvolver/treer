//! Offline closed-snapshot metadata import into exact preprovisioned paused bindings.
//! No bodies, history contexts, credentials, network, workers or activation authority.
use crate::{
    hub::Hub,
    private_store,
    store::{Error, Limits, Store},
    telegram_cutoff::{self, Cutoff, ResultMetadata},
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::{fs::MetadataExt, io::AsRawFd},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ROWS: usize = 1000;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub state_directory: PathBuf,
    pub hub_id: String,
    pub workspace_id: String,
    pub snapshot: PathBuf,
    pub snapshot_sha256: String,
    pub legacy_account: String,
    pub rollback: bool,
    pub cutoff: Cutoff,
}
fn invalid<T>() -> Result<T, Error> {
    Err(Error::Invalid)
}
fn file_identity(f: &File) -> Result<(u64, u64, u64, i64, i64), Error> {
    let m = f.metadata().map_err(|_| Error::PrivateStorage)?;
    Ok((m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec()))
}
fn digest(f: &mut File) -> Result<String, Error> {
    f.seek(SeekFrom::Start(0))
        .map_err(|_| Error::PrivateStorage)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut total = 0;
    loop {
        let n = f.read(&mut buf).map_err(|_| Error::PrivateStorage)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_BYTES {
            return invalid();
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
struct Snapshot {
    file: File,
    directory: rustix::fd::OwnedFd,
    name: std::ffi::OsString,
    identity: (u64, u64, u64, i64, i64),
    hash: String,
}
impl Snapshot {
    fn open(path: &Path, hash: &str) -> Result<Self, Error> {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return invalid();
        }
        let directory =
            private_store::open_directory(path.parent().ok_or(Error::PrivateStorage)?, false)?;
        let name = path.file_name().ok_or(Error::PrivateStorage)?.to_owned();
        let fd = rustix::fs::openat(
            &directory,
            &name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Error::PrivateStorage)?;
        private_store::check_file(&fd)?;
        let file = File::from(fd);
        let identity = file_identity(&file)?;
        if identity.2 == 0 || identity.2 > MAX_BYTES {
            return invalid();
        }
        let mut s = Self {
            file,
            directory,
            name,
            identity,
            hash: hash.into(),
        };
        s.recheck()?;
        Ok(s)
    }
    fn recheck(&mut self) -> Result<(), Error> {
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut name = self.name.clone();
            name.push(suffix);
            match rustix::fs::statat(
                &self.directory,
                &name,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Err(rustix::io::Errno::NOENT) => {}
                _ => return Err(Error::PrivateStorage),
            }
        }
        let m = rustix::fs::statat(
            &self.directory,
            &self.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| Error::PrivateStorage)?;
        if m.st_dev as u64 != self.identity.0
            || m.st_ino as u64 != self.identity.1
            || m.st_mode & 0o7777 != 0o600
            || m.st_nlink != 1
            || m.st_uid != rustix::process::geteuid().as_raw()
            || file_identity(&self.file)? != self.identity
            || digest(&mut self.file)? != self.hash
        {
            return Err(Error::PrivateStorage);
        }
        Ok(())
    }
    fn verify(&mut self, r: &Request) -> Result<(), Error> {
        // The descriptor is retained throughout the immutable, read-only SQLite
        // query. A second full fingerprint/identity check precedes target writes.
        let uri = format!("file:/dev/fd/{}?mode=ro&immutable=1", self.file.as_raw_fd());
        let db = rusqlite::Connection::open_with_flags(
            uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;
        let mut ticks = 0;
        db.progress_handler(
            1000,
            Some(move || {
                ticks += 1;
                ticks > 10000
            }),
        );
        db.execute_batch("PRAGMA query_only=ON;")?;
        let integrity: String = db.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if integrity != "ok" || version != 0 {
            return invalid();
        }
        for table in [
            "metadata",
            "poc_bindings",
            "im_users",
            "outbound_deliveries",
            "outbound_chunks",
            "processed_updates",
            "message_mappings",
        ] {
            let kind: Option<String> = db
                .query_row(
                    "SELECT type FROM sqlite_schema WHERE name=?1",
                    [table],
                    |row| row.get(0),
                )
                .optional()?;
            if kind.as_deref() != Some("table") {
                return invalid();
            }
        }
        let bot: String =
            db.query_row("SELECT value FROM metadata WHERE key='bot_id'", [], |row| {
                row.get(0)
            })?;
        let offset: Option<String> = db
            .query_row(
                "SELECT value FROM metadata WHERE key='last_update_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if bot.parse::<i64>().ok() != Some(r.cutoff.bot_id)
            || offset.as_deref().unwrap_or("-1").parse::<i64>().ok()
                != Some(r.cutoff.legacy_last_update_id)
        {
            return invalid();
        }
        for (table, terminal) in [
            ("outbound_deliveries", "acked"),
            ("outbound_chunks", "sent"),
            ("processed_updates", "completed"),
        ] {
            let mut q = db.prepare(&format!("SELECT status FROM {table} LIMIT 1001"))?;
            let rows = q
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if rows.len() > MAX_ROWS || rows.iter().any(|s| s != terminal) {
                return invalid();
            }
        }
        let historical: i64 = db.query_row("SELECT count(*) FROM message_mappings", [], |row| {
            row.get(0)
        })?;
        if !(0..=1000).contains(&historical) {
            return invalid();
        }
        let mut q=db.prepare("SELECT channel,account_id,conversation,target_agent_id,enabled FROM poc_bindings ORDER BY conversation LIMIT 21")?;
        let bindings = q
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if bindings.len() != r.cutoff.mappings.len() {
            return invalid();
        }
        let mut seen = std::collections::HashSet::new();
        for (channel, account, conversation, recipient, enabled) in bindings {
            if channel != "telegram"
                || account != r.legacy_account
                || ![0, 1].contains(&enabled)
                || !seen.insert(conversation.clone())
            {
                return invalid();
            }
            let matched = r
                .cutoff
                .mappings
                .iter()
                .filter(|m| {
                    format!("{}:{}", m.target.chat_id, m.target.topic_id.unwrap_or(0))
                        == conversation
                        && m.binding.recipient == recipient
                })
                .count();
            if matched != 1 {
                return invalid();
            }
        }
        let mut q =
            db.prepare("SELECT channel,account_id,user_id,enabled FROM im_users LIMIT 1001")?;
        let senders = q
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if senders.len() > MAX_ROWS {
            return invalid();
        }
        let mut allowed = std::collections::BTreeSet::new();
        for (channel, account, id, enabled) in senders {
            let id = id
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or(Error::Invalid)?;
            if channel != "telegram" || account != r.legacy_account || ![0, 1].contains(&enabled) {
                return invalid();
            }
            if enabled == 1 && !allowed.insert(id) {
                return invalid();
            }
        }
        for m in &r.cutoff.mappings {
            if m.target
                .allowed_senders
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                != allowed
            {
                return invalid();
            }
        }
        self.recheck()
    }
}
/// Existing private paused target bindings/pins are independently provisioned.
/// The import validates the *closed source* against all of them and commits its
/// fingerprint with offset/journal together. It does not copy history or secrets.
pub fn run_file(path: &Path) -> Result<ResultMetadata, Error> {
    let bytes = private_store::read_config(path)?;
    let r: Request = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
    if !crate::store::valid_id(&r.legacy_account)
        || !r.state_directory.join("hub.sqlite3").is_file()
    {
        return invalid();
    }
    let mut source = Snapshot::open(&r.snapshot, &r.snapshot_sha256)?;
    source.verify(&r)?;
    let store = Store::open(&r.state_directory, Limits::default())?;
    let mut hub = Hub::new(store, telegram_cutoff::Offline, &r.hub_id, &r.workspace_id)
        .map_err(|_| Error::Invalid)?;
    hub.telegram_schema()?;
    let fingerprint = r.cutoff.fingerprint()?;
    let binding=serde_json::to_vec(&serde_json::json!({"cutoff":&r.cutoff,"source_sha256":&r.snapshot_sha256,"legacy_account":&r.legacy_account})).map_err(|_|Error::Invalid)?;
    let import_hash: String = Sha256::digest(binding)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    hub.store.db.execute_batch("CREATE TABLE IF NOT EXISTS telegram_cutoff_journal(account TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,before_offset INTEGER NOT NULL,after_offset INTEGER NOT NULL,state TEXT NOT NULL CHECK(state IN ('applied','rolled_back')));CREATE TABLE IF NOT EXISTS telegram_snapshot_imports(account TEXT PRIMARY KEY,fingerprint TEXT NOT NULL);")?;
    let tx = hub
        .store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior: Option<String> = tx
        .query_row(
            "SELECT fingerprint FROM telegram_snapshot_imports WHERE account=?1",
            [&r.cutoff.account_id],
            |row| row.get(0),
        )
        .optional()?;
    if prior.as_ref().is_some_and(|p| p != &import_hash) || (r.rollback && prior.is_none()) {
        return invalid();
    }
    source.recheck()?;
    let result = telegram_cutoff::apply_in_transaction(&tx, &r.cutoff, r.rollback, &fingerprint)?;
    tx.execute(
        "INSERT OR IGNORE INTO telegram_snapshot_imports VALUES(?1,?2)",
        params![r.cutoff.account_id, import_hash],
    )?;
    tx.commit()?;
    Ok(result)
}

#[cfg(test)]
#[path = "telegram_snapshot_import_tests.rs"]
mod tests;
