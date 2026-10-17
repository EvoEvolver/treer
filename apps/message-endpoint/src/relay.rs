//! Endpoint relay: own Core mailbox, local human-provisioned pins, no transport secrets.
use crate::core::{Core, Delivery};
use crate::failures::FailureReason;
use crate::protocol::{self, Direction, Envelope, Fragment, Sender};
use crate::store::{Error as StoreError, Limits, Store};
use crate::transfers::FragmentReceipt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("relay_configuration_unavailable")]
    Configuration,
    #[error("relay_failure_budget_exhausted")]
    FailureBudget,
    #[error("relay_storage_unavailable")]
    Storage(#[from] StoreError),
    #[error("relay_core_unavailable")]
    Core(#[from] crate::core::Error),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub endpoint_id: String,
    pub hub_id: String,
    pub generation: i64,
    pub recipients: Vec<String>,
    pub cli: PathBuf,
    pub state_directory: PathBuf,
    pub release_sha256: String,
    pub cli_sha256: String,
}
impl Config {
    pub fn load(path: &Path, own_id: &str) -> Result<Self, Error> {
        let bytes = crate::private_store::read_config(path)?;
        let config: Self = serde_json::from_slice(&bytes).map_err(|_| Error::Configuration)?;
        config.validate(own_id)?;
        Ok(config)
    }
    fn verify_binaries(&self, running_executable: &Path) -> Result<String, Error> {
        let actual = hash_executable(running_executable)?;
        if actual != self.release_sha256 || hash_executable(&self.cli)? != self.cli_sha256 {
            return Err(Error::Configuration);
        }
        Ok(actual)
    }
    fn validate(&self, own_id: &str) -> Result<(), Error> {
        let ids = self
            .recipients
            .iter()
            .chain([&self.endpoint_id, &self.hub_id]);
        if own_id != self.endpoint_id
            || self.endpoint_id == self.hub_id
            || self.generation <= 0
            || !self.cli.is_absolute()
            || !self.state_directory.is_absolute()
            || self.recipients.is_empty()
            || self.recipients.len() > 32
            || self.recipients.iter().collect::<HashSet<_>>().len() != self.recipients.len()
            || !ids.into_iter().all(|id| agent_id(id))
            || !valid_hash(&self.release_sha256)
            || !valid_hash(&self.cli_sha256)
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }
}
pub const STARTUP_ERROR_BACKOFF_SECONDS: u64 = 30;
fn hash_executable(path: &Path) -> Result<String, Error> {
    use sha2::{Digest, Sha256};
    use std::{fs::File, io::Read};
    let mut file = File::open(path).map_err(|_| Error::Configuration)?;
    let mut hash = Sha256::new();
    let mut size = 0usize;
    let mut bytes = [0u8; 65536];
    loop {
        let read = file.read(&mut bytes).map_err(|_| Error::Configuration)?;
        if read == 0 {
            break;
        }
        size += read;
        if size > 64 * 1024 * 1024 {
            return Err(Error::Configuration);
        }
        hash.update(&bytes[..read]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn agent_id(id: &str) -> bool {
    id.split_once('_').is_some_and(|(prefix, suffix)| {
        matches!(prefix, "ag" | "app")
            && !suffix.is_empty()
            && suffix.len() <= 128
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// Small typed boundary so failure/crash ordering can be tested without a real
/// Controller. Core's adapter separately validates the real CLI argv contract.
pub trait Mailbox {
    fn receive(&self, wait_ms: u32) -> Result<Vec<Delivery>, crate::core::Error>;
    fn send(
        &self,
        to: &str,
        key: &str,
        body: &str,
        contexts: &[String],
        external: Option<Value>,
    ) -> Result<String, crate::core::Error>;
    fn ack(&self, delivery: &str) -> Result<(), crate::core::Error>;
}
impl Mailbox for Core {
    fn receive(&self, wait_ms: u32) -> Result<Vec<Delivery>, crate::core::Error> {
        Core::receive(self, wait_ms)
    }
    fn send(
        &self,
        to: &str,
        key: &str,
        body: &str,
        contexts: &[String],
        external: Option<Value>,
    ) -> Result<String, crate::core::Error> {
        Core::send(self, to, key, body, contexts, external)
    }
    fn ack(&self, delivery: &str) -> Result<(), crate::core::Error> {
        Core::ack(self, delivery)
    }
}

pub struct Relay<M> {
    config: Config,
    store: Store,
    mailbox: M,
}
impl Relay<Core> {
    pub fn open(config: Config, own_id: &str) -> Result<(Self, String), Error> {
        config.validate(own_id)?;
        let actual =
            config.verify_binaries(&std::env::current_exe().map_err(|_| Error::Configuration)?)?;
        let mailbox = Core::new(&config.cli, own_id)?;
        let store = Store::open(&config.state_directory, Limits::default())?;
        Ok((Self::provision(config, store, mailbox)?, actual))
    }
}
impl<M: Mailbox> Relay<M> {
    fn provision(config: Config, mut store: Store, mailbox: M) -> Result<Self, Error> {
        let pins =
            serde_json::to_string(&json!({"endpoint":config.endpoint_id,"hub":config.hub_id,
            "generation":config.generation,"recipients":config.recipients}))
            .map_err(|_| Error::Configuration)?;
        let hub: bool = store
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='hub_identity')",
                [],
                |r| r.get(0),
            )
            .map_err(StoreError::from)?;
        if hub {
            return Err(Error::Configuration);
        }
        let transaction = store.db.transaction().map_err(StoreError::from)?;
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS relay_pins (singleton INTEGER PRIMARY KEY CHECK(singleton=1), pins TEXT NOT NULL); CREATE TABLE IF NOT EXISTS relay_status (singleton INTEGER PRIMARY KEY CHECK(singleton=1), failure_budget_exhausted INTEGER NOT NULL DEFAULT 0); INSERT OR IGNORE INTO relay_status(singleton) VALUES(1)").map_err(StoreError::from)?;
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS relay_forwarded(transfer TEXT PRIMARY KEY,digest TEXT NOT NULL,recipient TEXT,accepted INTEGER NOT NULL DEFAULT 0,notified INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL);").map_err(StoreError::from)?;
        let existing: Option<String> = transaction
            .query_row("SELECT pins FROM relay_pins WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(StoreError::from)?;
        if let Some(existing) = existing {
            if existing != pins {
                return Err(Error::Configuration);
            }
        } else {
            transaction
                .execute(
                    "INSERT INTO relay_pins(singleton,pins) VALUES(1,?1)",
                    [pins],
                )
                .map_err(StoreError::from)?;
        }
        // This private file is human provisioning authority; no received message
        // can replace it or expand recipients. Existing state must match its pins.
        let existing: Option<(i64, String)> = transaction
            .query_row(
                "SELECT generation,recipient FROM endpoints WHERE id=?1",
                [&config.endpoint_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(StoreError::from)?;
        if let Some((generation, recipient)) = existing {
            if generation != config.generation || recipient != config.recipients[0] {
                return Err(Error::Configuration);
            }
        } else {
            let others: i64 = transaction
                .query_row("SELECT COUNT(*) FROM endpoints", [], |r| r.get(0))
                .map_err(StoreError::from)?;
            if others != 0 {
                return Err(Error::Configuration);
            }
            transaction
                .execute(
                    "INSERT INTO endpoints(id,generation,recipient,paused) VALUES (?1,?2,?3,1)",
                    rusqlite::params![config.endpoint_id, config.generation, config.recipients[0]],
                )
                .map_err(StoreError::from)?;
        }
        transaction
            .execute(
                "UPDATE endpoints SET paused=0 WHERE id=?1",
                [&config.endpoint_id],
            )
            .map_err(StoreError::from)?;
        transaction.commit().map_err(StoreError::from)?;
        Ok(Self {
            config,
            store,
            mailbox,
        })
    }

    /// Diagnostics only: outer Core sender authenticates the endpoint; neither
    /// a matching nor a differing hash proves command/profile integrity.
    pub fn report_binary(&self, actual: &str, boot_nonce: &str) -> Result<(), Error> {
        if !valid_hash(actual) || !crate::store::valid_id(boot_nonce) {
            return Err(Error::Configuration);
        }
        let state = if self.failure_budget_exhausted()? {
            "failure_budget_exhausted"
        } else {
            "running"
        };
        let body =
            json!({"protocol":"treer.endpoint.status/v1","state":state,"generation":self.config.generation,
            "release_sha256":self.config.release_sha256,"actual_sha256":actual,
            "version":env!("CARGO_PKG_VERSION")})
            .to_string();
        let key = protocol::digest_id(
            "startup-status",
            &self.config.endpoint_id,
            self.config.generation as u64,
            &format!("{boot_nonce}-{state}"),
        );
        self.mailbox.send(
            &self.config.hub_id,
            &format!("ep-status-{key}"),
            &body,
            &[],
            None,
        )?;
        Ok(())
    }

    pub fn tick(&mut self, now: i64) -> Result<(), Error> {
        // Every loop and before any capacity retry: wrong clock/permissions is
        // visible as a fixed error, and admissions/acks stop until corrected.
        self.store.recheck_private_storage()?;
        self.store.cleanup_transfers(now)?;
        self.store.prune_completed(now)?;
        self.store
            .db
            .execute(
                "DELETE FROM relay_forwarded WHERE accepted=1 AND created_at<=?1",
                [now - crate::transfers::TOMBSTONE_TTL],
            )
            .map_err(StoreError::from)?;
        if self.failure_budget_exhausted()? {
            return Err(Error::FailureBudget);
        }
        let mut failure = self.drain_ready(now).err();
        if matches!(failure, Some(Error::FailureBudget)) {
            return Err(Error::FailureBudget);
        }
        let deliveries = self.mailbox.receive(1000)?;
        for delivery in deliveries {
            if let Err(error) = self.handle(delivery, now) {
                if matches!(error, Error::FailureBudget) {
                    return Err(error);
                }
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        if let Err(error) = self.drain_ready(now) {
            if failure.is_none() {
                failure = Some(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn failure_budget_exhausted(&self) -> Result<bool, Error> {
        self.store
            .db
            .query_row(
                "SELECT failure_budget_exhausted FROM relay_status WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(StoreError::from)
            .map_err(Error::from)
    }
    fn persist_failure(
        &mut self,
        delivery: &str,
        body: &[u8],
        reason: FailureReason,
        now: i64,
    ) -> Result<(), Error> {
        match self.store.record_failure(
            &self.config.endpoint_id,
            self.config.generation,
            delivery,
            body,
            reason,
            now,
        ) {
            Err(StoreError::Full) => {
                self.store
                    .db
                    .execute(
                        "UPDATE relay_status SET failure_budget_exhausted=1 WHERE singleton=1",
                        [],
                    )
                    .map_err(StoreError::from)?;
                Err(Error::FailureBudget)
            }
            Err(error) => Err(error.into()),
            Ok(()) => Ok(()),
        }
    }
    fn reject(
        &mut self,
        delivery: &str,
        body: &str,
        reason: FailureReason,
        now: i64,
    ) -> Result<(), Error> {
        self.persist_failure(delivery, body.as_bytes(), reason, now)?;
        self.mailbox.ack(delivery)?;
        Ok(())
    }
    fn handle(&mut self, delivery: Delivery, now: i64) -> Result<(), Error> {
        let sender = Sender {
            kind: delivery.message.sender.kind,
            id: delivery.message.sender.id.clone(),
        };
        if protocol::from_hub(&sender, &self.config.hub_id) {
            let frame = match Fragment::parse(&delivery.message.body) {
                Ok(frame) => frame,
                Err(_) => {
                    return self.reject(
                        &delivery.delivery_id,
                        &delivery.message.body,
                        FailureReason::InvalidEnvelope,
                        now,
                    )
                }
            };
            match self.store.save_fragment(
                &self.config.endpoint_id,
                self.config.generation,
                Direction::Inbound,
                &delivery.message.body,
                now,
            ) {
                Ok(FragmentReceipt::Expired) => {
                    // save_fragment already committed a terminal transfer receipt.
                    self.persist_failure(
                        &delivery.delivery_id,
                        delivery.message.body.as_bytes(),
                        FailureReason::TransferExpired,
                        now,
                    )?;
                }
                Ok(_) => {}
                Err(StoreError::Invalid | StoreError::Conflict) => {
                    return self.reject(
                        &delivery.delivery_id,
                        &delivery.message.body,
                        FailureReason::FragmentConflict,
                        now,
                    )
                }
                Err(error) => return Err(error.into()),
            }
            // Fragment identity is revalidated by Store; no sender/body identity
            // field is used to select the inbound path. The complete job drains later.
            let _ = frame;
            self.mailbox.ack(&delivery.delivery_id)?;
        } else {
            let envelope = Envelope::Outbound {
                delivery_id: delivery.delivery_id.clone(),
                message_id: delivery.message.message_id,
                original_sender: sender,
                body: delivery.message.body.clone(),
                context_ids: delivery.message.context_ids,
            };
            let frames =
                match envelope.fragments(&self.config.endpoint_id, self.config.generation as u64) {
                    Ok(frames) => frames,
                    Err(_) => {
                        return self.reject(
                            &delivery.delivery_id,
                            &delivery.message.body,
                            FailureReason::InvalidEnvelope,
                            now,
                        )
                    }
                };
            let first = Fragment::parse(&frames[0]).map_err(|_| Error::Configuration)?;
            let provenance_recipient = match &envelope {
                Envelope::Outbound {
                    original_sender, ..
                } if original_sender.kind == protocol::SenderKind::Agent => {
                    Some(original_sender.id.as_str())
                }
                _ => None,
            };
            self.reserve_forwarded(&first, provenance_recipient, now)?;
            for wire in frames {
                let frame = Fragment::parse(&wire).map_err(|_| Error::Configuration)?;
                self.mailbox.send(
                    &self.config.hub_id,
                    &frame.idempotency_key(),
                    &wire,
                    &[],
                    None,
                )?;
            }
            self.store
                .db
                .execute(
                    "UPDATE relay_forwarded SET accepted=1 WHERE transfer=?1",
                    [&first.transfer_id],
                )
                .map_err(StoreError::from)?;
            // Core's acceptance of all fragments is durable. Ambiguous/failed
            // sends leave the original unacked and repeat identical keys/payloads.
            self.mailbox.ack(&delivery.delivery_id)?;
        }
        Ok(())
    }

    fn reserve_forwarded(
        &mut self,
        frame: &Fragment,
        recipient: Option<&str>,
        now: i64,
    ) -> Result<(), Error> {
        let tx = self.store.db.transaction().map_err(StoreError::from)?;
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT digest,recipient FROM relay_forwarded WHERE transfer=?1",
                [&frame.transfer_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(StoreError::from)?;
        if let Some((digest, old)) = existing {
            if digest != frame.payload_digest || old.as_deref() != recipient {
                return Err(Error::Configuration);
            }
        } else {
            let count: i64 = tx
                .query_row("SELECT COUNT(*) FROM relay_forwarded", [], |r| r.get(0))
                .map_err(StoreError::from)?;
            if count >= 10_000 {
                return Err(StoreError::Full.into());
            }
            tx.execute("INSERT INTO relay_forwarded(transfer,digest,recipient,created_at) VALUES(?1,?2,?3,?4)",rusqlite::params![frame.transfer_id,frame.payload_digest,recipient,now]).map_err(StoreError::from)?;
        }
        tx.commit().map_err(StoreError::from)?;
        Ok(())
    }

    fn drain_ready(&mut self, now: i64) -> Result<(), Error> {
        let ready = self.store.ready_transfers(
            &self.config.endpoint_id,
            self.config.generation,
            Direction::Inbound,
        )?;
        for transfer in ready.into_iter().take(1) {
            let envelope = self.store.transfer_payload(
                &self.config.endpoint_id,
                self.config.generation,
                Direction::Inbound,
                &transfer,
            )?;
            if protocol::check_recipient(&envelope, &self.config.recipients).is_err() {
                let bytes = serde_json::to_vec(&envelope).map_err(|_| Error::Configuration)?;
                self.persist_failure(
                    &format!("transfer-{transfer}"),
                    &bytes,
                    FailureReason::RecipientDenied,
                    now,
                )?;
            } else if let Envelope::FailureNotice {
                ref recipient,
                ref transfer_id,
                ..
            } = envelope
            {
                let prior:Option<i64>=self.store.db.query_row("SELECT notified FROM relay_forwarded WHERE transfer=?1 AND recipient=?2 AND accepted=1",rusqlite::params![transfer_id,recipient],|r|r.get(0)).optional().map_err(StoreError::from)?;
                // Unrecorded/unfinished transfers cannot be invented by the hub.
                // Already-notified transfers retire silently, without looping.
                if prior == Some(0) {
                    let body = envelope.failure_notice_body().ok_or(Error::Configuration)?;
                    self.mailbox.send(
                        recipient,
                        &format!("ep-notice-{transfer_id}"),
                        &body,
                        &[],
                        None,
                    )?;
                    self.store
                        .db
                        .execute(
                            "UPDATE relay_forwarded SET notified=1 WHERE transfer=?1",
                            [transfer_id],
                        )
                        .map_err(StoreError::from)?;
                }
            } else if let Envelope::Deliver {
                recipient,
                body,
                context_ids,
                source,
                ..
            } = envelope
            {
                let external = json!({"channel":source.channel,"account_id":source.account_id,
                    "conversation_id":source.conversation_id,"message_id":source.message_id,
                    "metadata":{"endpoint_id":self.config.endpoint_id,"binding_generation":self.config.generation.to_string()}});
                self.mailbox.send(
                    &recipient,
                    &format!("ep-deliver-{transfer}"),
                    &body,
                    &context_ids,
                    Some(external),
                )?;
            } else {
                return Err(Error::Configuration);
            }
            // A downstream accepted send OR a committed terminal failure permits
            // retiring the envelope. A failed send leaves it ready for retry.
            self.store.finish_transfer(
                &self.config.endpoint_id,
                self.config.generation,
                Direction::Inbound,
                &transfer,
                now,
            )?;
        }
        Ok(())
    }
}
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Message, Principal};
    use crate::protocol::{Channel, ExternalSource, SenderKind};
    use std::cell::{Cell, RefCell};
    const NOW: i64 = 5_000_000;
    type Sent = (String, String, String);
    #[derive(Default)]
    struct Fake {
        deliveries: RefCell<Vec<Delivery>>,
        sent: RefCell<Vec<Sent>>,
        externals: RefCell<Vec<Option<Value>>>,
        acked: RefCell<Vec<String>>,
        fail_send: Cell<bool>,
    }
    impl Mailbox for Fake {
        fn receive(&self, _wait: u32) -> Result<Vec<Delivery>, crate::core::Error> {
            Ok(std::mem::take(&mut *self.deliveries.borrow_mut()))
        }
        fn send(
            &self,
            to: &str,
            key: &str,
            body: &str,
            _context: &[String],
            _external: Option<Value>,
        ) -> Result<String, crate::core::Error> {
            self.externals.borrow_mut().push(_external);
            self.sent
                .borrow_mut()
                .push((to.into(), key.into(), body.into()));
            if self.fail_send.get() {
                return Err(crate::core::Error::Rejected);
            }
            Ok("msg_accepted".into())
        }
        fn ack(&self, id: &str) -> Result<(), crate::core::Error> {
            self.acked.borrow_mut().push(id.into());
            Ok(())
        }
    }
    fn config() -> Config {
        Config {
            endpoint_id: "ag_endpoint".into(),
            hub_id: "app_hub".into(),
            generation: 1,
            recipients: vec!["ag_target".into()],
            cli: "/fake/treer".into(),
            state_directory: "/fake/state".into(),
            release_sha256: "a".repeat(64),
            cli_sha256: "c".repeat(64),
        }
    }
    fn fixture() -> Relay<Fake> {
        Relay::provision(
            config(),
            Store::memory(Limits::default()).unwrap(),
            Fake::default(),
        )
        .unwrap()
    }
    fn delivery(id: &str, sender: &str, body: &str) -> Delivery {
        Delivery {
            delivery_id: id.into(),
            recipient: Principal {
                kind: SenderKind::Agent,
                id: "ag_endpoint".into(),
            },
            message: Message {
                message_id: format!("msg_{id}"),
                sender: Principal {
                    kind: SenderKind::Agent,
                    id: sender.into(),
                },
                body: body.into(),
                context_ids: vec![],
            },
        }
    }
    fn inbound(recipient: &str) -> Vec<String> {
        Envelope::Deliver {
            job_id: "job_external".into(),
            recipient: recipient.into(),
            body: "external body".into(),
            context_ids: vec![],
            source: ExternalSource {
                channel: Channel::Telegram,
                account_id: "bot_account".into(),
                conversation_id: "chat".into(),
                message_id: "external_id".into(),
            },
        }
        .fragments("ag_endpoint", 1)
        .unwrap()
    }
    #[test]
    fn notice_of_unrecorded_or_unfinished_transfer_is_dropped_without_sending() {
        let mut relay = fixture();
        for n in 0..2 {
            let outbound = Envelope::Outbound {
                delivery_id: format!("del_local_{n}"),
                message_id: format!("msg_local_{n}"),
                original_sender: Sender {
                    kind: SenderKind::Agent,
                    id: "ag_target".into(),
                },
                body: "original".into(),
                context_ids: vec![],
            };
            let frame = Fragment::parse(&outbound.fragments("ag_endpoint", 1).unwrap()[0]).unwrap();
            if n == 1 {
                relay
                    .reserve_forwarded(&frame, Some("ag_target"), NOW)
                    .unwrap();
            }
            let notice = Envelope::FailureNotice {
                job_id: format!("notice_{n}"),
                recipient: "ag_target".into(),
                transfer_id: frame.transfer_id,
                code: protocol::NoticeCode::TransportRejected,
                attempts: 1,
            };
            let wire = notice.fragments("ag_endpoint", 1).unwrap().remove(0);
            relay
                .handle(delivery(&format!("del_notice_{n}"), "app_hub", &wire), NOW)
                .unwrap();
            relay.drain_ready(NOW).unwrap();
        }
        assert!(relay.mailbox.sent.borrow().is_empty());
    }
    #[test]
    fn real_outbound_records_notice_authority_before_ack_and_duplicate_notice_cannot_spam() {
        let mut relay = fixture();
        relay
            .handle(delivery("del_original", "ag_target", "original"), NOW)
            .unwrap();
        let frame = Fragment::parse(&relay.mailbox.sent.borrow()[0].2).unwrap();
        let accepted: i64 = relay
            .store
            .db
            .query_row(
                "SELECT accepted FROM relay_forwarded WHERE transfer=?1",
                [&frame.transfer_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(accepted, 1);
        assert_eq!(relay.mailbox.acked.borrow().as_slice(), ["del_original"]);
        relay.mailbox.sent.borrow_mut().clear();
        relay.mailbox.externals.borrow_mut().clear();
        for n in 0..2 {
            let env = Envelope::FailureNotice {
                job_id: format!("notice_wrapper_{n}"),
                recipient: "ag_target".into(),
                transfer_id: frame.transfer_id.clone(),
                code: protocol::NoticeCode::TransportRejected,
                attempts: n + 1,
            };
            let wire = env.fragments("ag_endpoint", 1).unwrap().remove(0);
            relay
                .handle(delivery(&format!("del_notice_{n}"), "app_hub", &wire), NOW)
                .unwrap();
            if n == 0 {
                relay.mailbox.fail_send.set(true);
                assert!(relay.drain_ready(NOW).is_err());
                relay.mailbox.fail_send.set(false);
            }
            relay.drain_ready(NOW).unwrap();
        }
        let sent = relay.mailbox.sent.borrow();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], sent[1]);
        assert!(sent[0].1.ends_with(&frame.transfer_id));
        assert!(relay.mailbox.externals.borrow().iter().all(Option::is_none));
    }
    #[test]
    fn provenance_budget_refuses_outbound_before_core_send_or_ack() {
        let mut relay = fixture();
        relay.store.db.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO relay_forwarded(transfer,digest,created_at) SELECT 'synthetic-'||i,'synthetic',0 FROM n;").unwrap();
        assert!(matches!(
            relay.handle(delivery("del_cap", "ag_target", "original"), NOW),
            Err(Error::Storage(StoreError::Full))
        ));
        assert!(relay.mailbox.sent.borrow().is_empty());
        assert!(relay.mailbox.acked.borrow().is_empty());
    }
    #[test]
    fn failure_notice_requires_hub_and_local_recipient_pin_and_has_no_external_identity() {
        for recipient in ["ag_target", "ag_outside"] {
            let mut relay = fixture();
            let outgoing = Envelope::Outbound {
                delivery_id: "del_outgoing".into(),
                message_id: "msg_outgoing".into(),
                original_sender: protocol::Sender {
                    kind: SenderKind::Agent,
                    id: "ag_target".into(),
                },
                body: "original".into(),
                context_ids: vec![],
            };
            let forwarded =
                Fragment::parse(&outgoing.fragments("ag_endpoint", 1).unwrap()[0]).unwrap();
            relay
                .reserve_forwarded(&forwarded, Some("ag_target"), NOW)
                .unwrap();
            relay
                .store
                .db
                .execute("UPDATE relay_forwarded SET accepted=1", [])
                .unwrap();
            let env = Envelope::FailureNotice {
                job_id: "failure_synthetic".into(),
                recipient: recipient.into(),
                transfer_id: forwarded.transfer_id,
                code: protocol::NoticeCode::TransportRejected,
                attempts: 2,
            };
            let wire = env.fragments("ag_endpoint", 1).unwrap().remove(0);
            relay
                .handle(delivery("del_notice", "app_hub", &wire), NOW)
                .unwrap();
            relay.drain_ready(NOW).unwrap();
            let sent = relay.mailbox.sent.borrow();
            if recipient == "ag_target" {
                assert_eq!(sent.len(), 1);
                assert_eq!(sent[0].0, "ag_target");
                assert!(sent[0].2.starts_with("[Treer endpoint transport status]"));
                assert!(relay.mailbox.externals.borrow()[0].is_none());
            } else {
                assert!(sent.is_empty());
            }
        }
        let mut relay = fixture();
        let body = serde_json::to_string(&Envelope::FailureNotice {
            job_id: "failure_fake".into(),
            recipient: "ag_outside".into(),
            transfer_id: "b".repeat(64),
            code: protocol::NoticeCode::TransportRejected,
            attempts: 1,
        })
        .unwrap();
        relay
            .handle(delivery("del_fake_notice", "ag_user", &body), NOW)
            .unwrap();
        assert_eq!(relay.mailbox.sent.borrow()[0].0, "app_hub"); // ordinary outbound text
    }
    #[test]
    fn outward_send_failure_never_acks_and_retries_identical_fragments() {
        let mut relay = fixture();
        relay.mailbox.fail_send.set(true);
        assert!(relay
            .handle(delivery("del_user", "ag_sender", "outbound body"), NOW)
            .is_err());
        assert!(relay.mailbox.acked.borrow().is_empty());
        relay.mailbox.fail_send.set(false);
        relay
            .handle(delivery("del_user", "ag_sender", "outbound body"), NOW + 1)
            .unwrap();
        let sent = relay.mailbox.sent.borrow();
        assert_eq!(sent[0], sent[1]);
        assert_eq!(sent[0].0, "app_hub");
        assert_eq!(*relay.mailbox.acked.borrow(), vec!["del_user"]);
    }
    #[test]
    fn forged_hub_body_is_forwarded_as_ordinary_content_and_bad_hub_is_recorded_before_ack() {
        let mut relay = fixture();
        let wire = inbound("ag_target").remove(0);
        relay
            .handle(delivery("del_forged", "ag_other", &wire), NOW)
            .unwrap();
        let sent = relay.mailbox.sent.borrow();
        assert_eq!(sent.len(), 1);
        let frame = Fragment::parse(&sent[0].2).unwrap();
        assert!(frame.direction == Direction::Outbound);
        drop(sent);
        relay
            .handle(delivery("del_poison", "app_hub", "not a fragment"), NOW)
            .unwrap();
        assert_eq!(
            relay
                .store
                .recent_failures("ag_endpoint", 1, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(relay.mailbox.acked.borrow().contains(&"del_poison".into()));
    }
    #[test]
    fn inbound_is_durable_before_ack_and_failed_send_replays_same_key_after_restart() {
        let mut relay = fixture();
        let wire = inbound("ag_target").remove(0);
        relay
            .handle(delivery("del_inbound", "app_hub", &wire), NOW)
            .unwrap();
        assert_eq!(*relay.mailbox.acked.borrow(), vec!["del_inbound"]);
        relay.mailbox.fail_send.set(true);
        assert!(relay.drain_ready(NOW).is_err());
        let Relay { config, store, .. } = relay;
        let mailbox = Fake::default();
        let mut relay = Relay::provision(config, store, mailbox).unwrap();
        relay.drain_ready(NOW + 1).unwrap();
        assert_eq!(relay.mailbox.sent.borrow().len(), 1);
        assert_eq!(relay.mailbox.sent.borrow()[0].0, "ag_target");
        assert!(relay.mailbox.sent.borrow()[0].1.starts_with("ep-deliver-"));
        relay.drain_ready(NOW + 2).unwrap();
        assert_eq!(relay.mailbox.sent.borrow().len(), 1);
    }
    #[test]
    fn recipient_denial_records_terminal_failure_without_sending() {
        let mut relay = fixture();
        let wire = inbound("ag_outside_allowlist").remove(0);
        relay
            .handle(delivery("del_denied", "app_hub", &wire), NOW)
            .unwrap();
        relay.drain_ready(NOW).unwrap();
        assert!(relay.mailbox.sent.borrow().is_empty());
        let failures = relay.store.recent_failures("ag_endpoint", 1, 10).unwrap();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason == FailureReason::RecipientDenied);
    }
    #[test]
    fn failed_inbound_does_not_starve_outbound_and_clock_errors_are_visible() {
        let mut relay = fixture();
        relay
            .handle(
                delivery("del_inbound", "app_hub", &inbound("ag_target").remove(0)),
                NOW,
            )
            .unwrap();
        relay.mailbox.fail_send.set(true);
        relay
            .mailbox
            .deliveries
            .borrow_mut()
            .push(delivery("del_outbound", "ag_sender", "body"));
        assert!(relay.tick(NOW).is_err());
        let sent = relay.mailbox.sent.borrow();
        assert!(sent.iter().any(|s| s.0 == "app_hub"));
        assert!(!relay
            .mailbox
            .acked
            .borrow()
            .contains(&"del_outbound".into()));
        drop(sent);
        assert!(relay.tick(0).is_err());
    }
    #[test]
    fn local_pin_changes_require_human_drain_and_new_state() {
        let relay = fixture();
        let Relay {
            config: mut changed,
            store,
            mailbox,
        } = relay;
        changed.hub_id = "app_replacement".into();
        assert!(matches!(
            Relay::provision(changed, store, mailbox),
            Err(Error::Configuration)
        ));
        let mut invalid = config();
        invalid.recipients.push("ag_target".into());
        assert!(invalid.validate("ag_endpoint").is_err());
        assert!(config().validate("ag_other").is_err());
    }
    #[test]
    fn startup_hash_is_status_only_and_does_not_change_recipient_pins() {
        let relay = fixture();
        relay.report_binary(&"b".repeat(64), "boot_nonce").unwrap();
        let sent = relay.mailbox.sent.borrow();
        let status: Value = serde_json::from_str(&sent[0].2).unwrap();
        assert_eq!(status["protocol"], "treer.endpoint.status/v1");
        assert_eq!(status["release_sha256"], "a".repeat(64));
        assert_eq!(status["actual_sha256"], "b".repeat(64));
        assert_eq!(relay.config.recipients, vec!["ag_target"]);
    }
    #[test]
    fn failure_budget_exhaustion_is_persistently_paused_without_ack() {
        let mut relay = fixture();
        for index in 0..1000 {
            relay
                .store
                .record_failure(
                    "ag_endpoint",
                    1,
                    &format!("del_{index}"),
                    b"bad",
                    FailureReason::InvalidEnvelope,
                    NOW,
                )
                .unwrap();
        }
        assert!(matches!(
            relay.handle(delivery("del_overflow", "app_hub", "bad"), NOW),
            Err(Error::FailureBudget)
        ));
        assert!(relay.mailbox.acked.borrow().is_empty());
        assert!(relay.failure_budget_exhausted().unwrap());
        assert!(matches!(relay.tick(NOW + 1), Err(Error::FailureBudget)));
        relay.report_binary(&"b".repeat(64), "boot_nonce").unwrap();
        let status: Value = serde_json::from_str(&relay.mailbox.sent.borrow()[0].2).unwrap();
        assert_eq!(status["state"], "failure_budget_exhausted");
    }
    #[test]
    fn local_config_requires_private_regular_file_and_matching_injected_id() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = base.join("endpoint.json");
        let value = json!({"endpoint_id":"ag_endpoint","hub_id":"app_hub","generation":1,
            "recipients":["ag_target"],"cli":"/fake/treer","state_directory":base.join("state"),"release_sha256":"a".repeat(64),"cli_sha256":"c".repeat(64)});
        std::fs::write(&path, value.to_string()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        Config::load(&path, "ag_endpoint").unwrap();
        assert!(Config::load(&path, "ag_different").is_err());
        let link = base.join("link.json");
        symlink(&path, &link).unwrap();
        assert!(Config::load(&link, "ag_endpoint").is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Config::load(&path, "ag_endpoint").is_err());
    }
    #[test]
    fn scheduler_runs_expiry_and_pruning_before_next_receive() {
        let mut relay = fixture();
        let long = Envelope::Deliver {
            job_id: "job_expiring".into(),
            recipient: "ag_target".into(),
            body: "a".repeat(30000),
            context_ids: vec![],
            source: ExternalSource {
                channel: Channel::Http,
                account_id: "account".into(),
                conversation_id: "conversation".into(),
                message_id: "external".into(),
            },
        }
        .fragments("ag_endpoint", 1)
        .unwrap();
        relay
            .handle(delivery("del_partial", "app_hub", &long[0]), NOW)
            .unwrap();
        relay
            .store
            .record_failure(
                "ag_endpoint",
                1,
                "del_old",
                b"bad",
                FailureReason::InvalidEnvelope,
                NOW - crate::transfers::TOMBSTONE_TTL,
            )
            .unwrap();
        relay.tick(NOW + crate::transfers::INCOMPLETE_TTL).unwrap();
        assert_eq!(
            relay
                .store
                .db
                .query_row("SELECT state FROM transfers", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert!(relay
            .store
            .recent_failures("ag_endpoint", 1, 10)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn binary_pins_stop_accidental_relay_and_cli_drift_before_execution() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temporary.path().join("relay");
        let cli = temporary.path().join("treer");
        std::fs::write(&binary, b"synthetic relay bytes").unwrap();
        std::fs::write(&cli, b"synthetic CLI bytes").unwrap();
        let mut config = config();
        config.cli = cli.clone();
        config.release_sha256 = hash_executable(&binary).unwrap();
        config.cli_sha256 = hash_executable(&cli).unwrap();
        assert_eq!(
            config.verify_binaries(&binary).unwrap(),
            config.release_sha256
        );
        std::fs::write(&binary, b"drift").unwrap();
        assert!(config.verify_binaries(&binary).is_err());
        std::fs::write(&binary, b"synthetic relay bytes").unwrap();
        std::fs::write(&cli, b"drift").unwrap();
        assert!(config.verify_binaries(&binary).is_err());
    }
    #[test]
    fn hundred_ready_inbound_jobs_do_not_starve_an_outbound_message() {
        let mut relay = fixture();
        for index in 0..100 {
            let envelope = Envelope::Deliver {
                job_id: format!("job_{index}"),
                recipient: "ag_target".into(),
                body: "external body".into(),
                context_ids: vec![],
                source: ExternalSource {
                    channel: Channel::Http,
                    account_id: "account".into(),
                    conversation_id: "conversation".into(),
                    message_id: format!("external_{index}"),
                },
            };
            let wire = envelope.fragments("ag_endpoint", 1).unwrap().remove(0);
            relay
                .handle(
                    delivery(&format!("del_inbound_{index}"), "app_hub", &wire),
                    NOW,
                )
                .unwrap();
        }
        relay.mailbox.deliveries.borrow_mut().push(delivery(
            "del_outbound",
            "ag_sender",
            "outbound body",
        ));
        relay.tick(NOW + 1).unwrap();
        let sent = relay.mailbox.sent.borrow();
        assert_eq!(sent.iter().filter(|s| s.0 == "ag_target").count(), 2);
        assert_eq!(sent.iter().filter(|s| s.0 == "app_hub").count(), 1);
        assert!(relay
            .mailbox
            .acked
            .borrow()
            .contains(&"del_outbound".into()));
        assert_eq!(
            relay
                .store
                .ready_transfers("ag_endpoint", 1, Direction::Inbound)
                .unwrap()
                .len(),
            98
        );
    }
}
