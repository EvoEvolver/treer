//! Offline, stopped-state cutoff journal. No polling, token loading or activation.
//! The inventory digest and drain/stop evidence are operator attestations, not
//! independent Core/OS verification. Existing paused bindings must match exactly.
use crate::{
    hub::{Binding, Directory, Hub},
    relay::Mailbox,
    store::Error,
    telegram::Target,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub binding: Binding,
    pub revision: i64,
    pub target: Target,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cutoff {
    pub inventory_sha256: String,
    pub bot_id: i64,
    pub account_id: String,
    pub expected_offset: i64,
    pub legacy_last_update_id: i64,
    pub mappings: Vec<Mapping>,
    pub history_loss_accepted: bool,
    pub app_asserted_receipts_accepted: bool,
    pub old_poller_stopped: bool,
    pub senders_fenced: bool,
    pub old_mailbox_zero_samples: [i64; 2],
    pub legacy_outbox_drained: bool,
}
#[derive(Serialize)]
pub struct ResultMetadata {
    pub state: &'static str,
    pub next_offset: i64,
    pub fingerprint: String,
    pub activation_authorized: bool,
}
fn invalid<T>() -> Result<T, Error> {
    Err(Error::Invalid)
}
impl Cutoff {
    fn fingerprint(&self) -> Result<String, Error> {
        if self.inventory_sha256.len() != 64
            || !self
                .inventory_sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || self.bot_id <= 0
            || !crate::store::valid_id(&self.account_id)
            || self.expected_offset < 0
            || self.legacy_last_update_id < -1
            || self.legacy_last_update_id > i64::MAX - 2
            || self.legacy_last_update_id + 1 < self.expected_offset
            || self.mappings.is_empty()
            || self.mappings.len() > 20
            || !self.history_loss_accepted
            || !self.app_asserted_receipts_accepted
            || !self.old_poller_stopped
            || !self.senders_fenced
            || !self.legacy_outbox_drained
            || self.old_mailbox_zero_samples[0] <= 0
            || self.old_mailbox_zero_samples[1].saturating_sub(self.old_mailbox_zero_samples[0]) < 2
        {
            return invalid();
        }
        let bytes = serde_json::to_vec(self).map_err(|_| Error::Invalid)?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
}
/// The caller must hold the real Store's process lock, with all workers stopped.
/// No method verifies a caller-supplied assertion or grants live cutover authority.
pub fn apply<M: Mailbox + Directory>(
    hub: &mut Hub<M>,
    cutoff: &Cutoff,
    rollback: bool,
) -> Result<ResultMetadata, Error> {
    let fingerprint = cutoff.fingerprint()?;
    hub.store.recheck_private_storage()?;
    hub.telegram_schema()?;
    hub.store.db.execute_batch("CREATE TABLE IF NOT EXISTS telegram_cutoff_journal(account TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,before_offset INTEGER NOT NULL,after_offset INTEGER NOT NULL,state TEXT NOT NULL CHECK(state IN ('applied','rolled_back')));")?;
    let tx = hub
        .store
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let bot: i64 = tx.query_row(
        "SELECT bot_id FROM telegram_bot_pins WHERE account=?1",
        [&cutoff.account_id],
        |r| r.get(0),
    )?;
    if bot != cutoff.bot_id {
        return invalid();
    }
    let offset: i64 = tx.query_row(
        "SELECT offset FROM telegram_accounts WHERE account=?1",
        [&cutoff.account_id],
        |r| r.get(0),
    )?;
    let journal: Option<(String, String)> = tx
        .query_row(
            "SELECT fingerprint,state FROM telegram_cutoff_journal WHERE account=?1",
            [&cutoff.account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let next = cutoff.legacy_last_update_id + 1;
    if journal.as_ref().is_some_and(|(f, _)| f != &fingerprint) {
        return invalid();
    }
    let target_count: i64 = tx.query_row(
        "SELECT count(*) FROM telegram_targets WHERE account=?1",
        [&cutoff.account_id],
        |r| r.get(0),
    )?;
    if target_count != cutoff.mappings.len() as i64 {
        return invalid();
    }
    let mut endpoints = std::collections::HashSet::new();
    for m in &cutoff.mappings {
        if !endpoints.insert(&m.binding.endpoint_id) || m.target.account_id != cutoff.account_id {
            return invalid();
        }
        let (definition,revision,paused,target):(String,i64,bool,String)=tx.query_row("SELECT b.definition,b.revision,e.paused,t.definition FROM hub_bindings b JOIN endpoints e ON e.id=b.endpoint JOIN telegram_targets t ON t.endpoint=e.id WHERE e.id=?1",[&m.binding.endpoint_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let binding: Binding = serde_json::from_str(&definition).map_err(|_| Error::Invalid)?;
        let actual: Target = serde_json::from_str(&target).map_err(|_| Error::Invalid)?;
        if binding != m.binding || actual != m.target || revision != m.revision || !paused {
            return invalid();
        }
        // No rollback across activation, ignored updates, new traffic, sends or
        // completed work. This deliberately permits only pre-traffic reversal.
        let traffic:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM hub_activity WHERE endpoint=?1) OR EXISTS(SELECT 1 FROM inbound WHERE endpoint=?1) OR EXISTS(SELECT 1 FROM outbound WHERE endpoint=?1) OR EXISTS(SELECT 1 FROM transfers WHERE endpoint=?1) OR EXISTS(SELECT 1 FROM telegram_chunks WHERE endpoint=?1)",[&m.binding.endpoint_id],|r|r.get(0))?;
        if traffic {
            return invalid();
        }
    }
    let ignored: i64 = tx.query_row(
        "SELECT ignored FROM telegram_accounts WHERE account=?1",
        [&cutoff.account_id],
        |r| r.get(0),
    )?;
    if ignored != 0 {
        return invalid();
    }
    let expected = if rollback {
        next
    } else {
        cutoff.expected_offset
    };
    let desired = if rollback {
        cutoff.expected_offset
    } else {
        next
    };
    let state = if rollback { "rolled_back" } else { "applied" };
    match journal {
        Some((_, old)) if old == state => {
            if offset != desired {
                return invalid();
            }
        }
        Some((_, old)) if rollback && old == "applied" => {
            if offset != expected {
                return invalid();
            }
            tx.execute(
                "UPDATE telegram_accounts SET offset=?2 WHERE account=?1",
                params![cutoff.account_id, desired],
            )?;
            tx.execute(
                "UPDATE telegram_cutoff_journal SET state='rolled_back' WHERE account=?1",
                [&cutoff.account_id],
            )?;
        }
        None if !rollback => {
            if offset != expected {
                return invalid();
            }
            tx.execute(
                "UPDATE telegram_accounts SET offset=?2 WHERE account=?1",
                params![cutoff.account_id, desired],
            )?;
            tx.execute(
                "INSERT INTO telegram_cutoff_journal VALUES(?1,?2,?3,?4,'applied')",
                params![cutoff.account_id, fingerprint, expected, desired],
            )?;
        }
        _ => return invalid(),
    }
    tx.commit()?;
    Ok(ResultMetadata {
        state,
        next_offset: desired,
        fingerprint,
        activation_authorized: false,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub state_directory: std::path::PathBuf,
    pub hub_id: String,
    pub workspace_id: String,
    pub rollback: bool,
    pub cutoff: Cutoff,
}
/// A local operator invocation reads only the named private request/store. No
/// environment credentials, account token files, CLI child or network are used.
pub fn run_file(path: &std::path::Path) -> Result<ResultMetadata, Error> {
    let bytes = crate::private_store::read_config(path)?;
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
    if !request.state_directory.join("hub.sqlite3").is_file() {
        return invalid();
    }
    let store =
        crate::store::Store::open(&request.state_directory, crate::store::Limits::default())?;
    let mut hub = Hub::new(store, Offline, &request.hub_id, &request.workspace_id)
        .map_err(|_| Error::Invalid)?;
    apply(&mut hub, &request.cutoff, request.rollback)
}
#[cfg(test)]
#[path = "telegram_cutoff_tests.rs"]
mod tests;

struct Offline;
impl Directory for Offline {
    fn metadata(&self, _: &str) -> Result<crate::core::AgentMetadata, crate::core::Error> {
        Err(crate::core::Error::Unavailable)
    }
}
impl Mailbox for Offline {
    fn receive(&self, _: u32) -> Result<Vec<crate::core::Delivery>, crate::core::Error> {
        Err(crate::core::Error::Unavailable)
    }
    fn send(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<serde_json::Value>,
    ) -> Result<String, crate::core::Error> {
        Err(crate::core::Error::Unavailable)
    }
    fn ack(&self, _: &str) -> Result<(), crate::core::Error> {
        Err(crate::core::Error::Unavailable)
    }
}
