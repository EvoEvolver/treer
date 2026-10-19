//! Local stage-2 storage, not yet dispatched by the runtime. There is no
//! authorization here: expected handoff and outer Core principal/workspace must
//! be checked by the eventual caller. Parsed receipts remain App assertions.
use crate::{
    store::{Error, Store},
    telegram_receipts::{Expected, Receipt, Scope},
};
use rusqlite::{params, OptionalExtension};

const PER_BINDING_ROWS: i64 = 1000;
const GLOBAL_ROWS: i64 = 10_000;

#[derive(Debug, PartialEq, Eq)]
pub enum Recorded {
    Inserted,
    Replay,
}

/// No automatic expiry: retain mappings until reviewed binding retirement.
/// At capacity, refuse new mappings instead of erasing an old reply context.
pub struct Mappings<'a> {
    store: &'a mut Store,
}
impl<'a> Mappings<'a> {
    pub fn new(store: &'a mut Store) -> Result<Self, Error> {
        store.db.execute_batch(
            "CREATE TABLE IF NOT EXISTS telegram_reply_mappings(
                account TEXT NOT NULL,chat INTEGER NOT NULL,topic INTEGER NOT NULL,
                endpoint TEXT NOT NULL,generation INTEGER NOT NULL,recipient TEXT NOT NULL,
                native_id INTEGER NOT NULL,core_id TEXT NOT NULL,job TEXT NOT NULL,
                digest TEXT NOT NULL,created_at INTEGER NOT NULL,
                PRIMARY KEY(account,chat,topic,endpoint,generation,recipient,native_id),
                UNIQUE(endpoint,generation,job));
             CREATE TABLE IF NOT EXISTS telegram_reply_counters(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                conflicts INTEGER NOT NULL DEFAULT 0);
             INSERT OR IGNORE INTO telegram_reply_counters(singleton) VALUES(1);",
        )?;
        Ok(Self { store })
    }

    /// The caller keeps a conflicting handoff unacknowledged/held. Only an
    /// aggregate conflict counter is recorded; no receipt body or IDs in audit.
    pub fn record(
        &mut self,
        wire: &[u8],
        outer_core_sender: &str,
        expected: Expected<'_>,
        now: i64,
    ) -> Result<Recorded, Error> {
        if now < 0 {
            return Err(Error::Invalid);
        }
        let r =
            Receipt::parse_for(wire, outer_core_sender, expected).map_err(|_| Error::Invalid)?;
        let s = &r.scope;
        let tx = self.store.db.transaction()?;
        let prior: Option<(String, String, String)> = tx
            .query_row(
                "SELECT core_id,job,digest FROM telegram_reply_mappings
             WHERE account=?1 AND chat=?2 AND topic=?3 AND endpoint=?4
             AND generation=?5 AND recipient=?6 AND native_id=?7",
                params![
                    s.account_id,
                    s.chat_id,
                    s.topic_id.unwrap_or(0),
                    s.endpoint_id,
                    s.generation,
                    s.recipient_id,
                    r.native_message_id
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((core, job, digest)) = prior {
            if core == r.final_core_message_id && job == r.job_id && digest == r.payload_digest {
                tx.commit()?;
                return Ok(Recorded::Replay);
            }
        } else {
            let reused: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM telegram_reply_mappings
                 WHERE endpoint=?1 AND generation=?2 AND job=?3)",
                params![s.endpoint_id, s.generation, r.job_id],
                |row| row.get(0),
            )?;
            if !reused {
                let total: i64 =
                    tx.query_row("SELECT count(*) FROM telegram_reply_mappings", [], |row| {
                        row.get(0)
                    })?;
                let binding: i64 = tx.query_row(
                    "SELECT count(*) FROM telegram_reply_mappings
                     WHERE endpoint=?1 AND generation=?2",
                    params![s.endpoint_id, s.generation],
                    |row| row.get(0),
                )?;
                if total >= GLOBAL_ROWS || binding >= PER_BINDING_ROWS {
                    return Err(Error::Full);
                }
                tx.execute(
                    "INSERT INTO telegram_reply_mappings
                     (account,chat,topic,endpoint,generation,recipient,native_id,core_id,job,digest,created_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![s.account_id,s.chat_id,s.topic_id.unwrap_or(0),s.endpoint_id,
                        s.generation,s.recipient_id,r.native_message_id,r.final_core_message_id,
                        r.job_id,r.payload_digest,now],
                )?;
                tx.commit()?;
                return Ok(Recorded::Inserted);
            }
        }
        tx.execute(
            "UPDATE telegram_reply_counters SET conflicts=CASE
             WHEN conflicts<9223372036854775807 THEN conflicts+1 ELSE conflicts END
             WHERE singleton=1",
            [],
        )?;
        tx.commit()?;
        Err(Error::Conflict)
    }

    /// The returned ID is not proof of readability or delivery. Any subsequent
    /// context-bearing send must retain Core visibility checks and hold on error.
    /// An unknown/old reply is unavailable; callers must not silently omit it.
    pub fn context(&self, scope: &Scope, native_id: i64) -> Result<String, Error> {
        if !scope.valid() || native_id <= 0 {
            return Err(Error::Invalid);
        }
        self.store
            .db
            .query_row(
                "SELECT core_id FROM telegram_reply_mappings
             WHERE account=?1 AND chat=?2 AND topic=?3 AND endpoint=?4
             AND generation=?5 AND recipient=?6 AND native_id=?7",
                params![
                    scope.account_id,
                    scope.chat_id,
                    scope.topic_id.unwrap_or(0),
                    scope.endpoint_id,
                    scope.generation,
                    scope.recipient_id,
                    native_id
                ],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::Unavailable)
    }

    pub fn conflict_count(&self) -> Result<i64, Error> {
        Ok(self.store.db.query_row(
            "SELECT conflicts FROM telegram_reply_counters WHERE singleton=1",
            [],
            |row| row.get(0),
        )?)
    }
}

/// Normalize the transport/conversation sentinel once. Wire receipts themselves
/// require None or a positive topic, so Some(0) cannot create a second scope.
pub fn normalize_transport_topic(topic: Option<i64>) -> Result<Option<i64>, Error> {
    match topic {
        None | Some(0) => Ok(None),
        Some(id) if id > 0 => Ok(Some(id)),
        _ => Err(Error::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{store::Limits, telegram_receipts::CAPABILITY};
    use serde_json::json;

    fn scope() -> Scope {
        Scope {
            account_id: "synthetic".into(),
            chat_id: -123,
            topic_id: None,
            endpoint_id: "ag_relay".into(),
            generation: 1,
            recipient_id: "ag_recipient".into(),
        }
    }
    fn record(
        m: &mut Mappings<'_>,
        s: &Scope,
        native: i64,
        job: &str,
        core: &str,
    ) -> Result<Recorded, Error> {
        let digest = "a".repeat(64);
        let body = serde_json::to_vec(&json!({
            "protocol":CAPABILITY,"verification":"app_asserted","scope":s,
            "job_id":job,"payload_digest":digest,"native_message_id":native,
            "final_core_message_id":core,
        }))
        .unwrap();
        m.record(
            &body,
            &s.endpoint_id,
            Expected {
                scope: s,
                job_id: job,
                payload_digest: &digest,
                native_message_id: native,
            },
            42,
        )
    }
    #[test]
    fn replay_is_noop_and_changed_final_id_cannot_replace_mapping() {
        let mut store = Store::memory(Limits::default()).unwrap();
        let mut m = Mappings::new(&mut store).unwrap();
        assert_eq!(
            record(&mut m, &scope(), 11, "job_one", "msg_first").unwrap(),
            Recorded::Inserted
        );
        assert_eq!(
            record(&mut m, &scope(), 11, "job_one", "msg_first").unwrap(),
            Recorded::Replay
        );
        assert!(matches!(
            record(&mut m, &scope(), 11, "job_one", "msg_other"),
            Err(Error::Conflict)
        ));
        assert_eq!(m.context(&scope(), 11).unwrap(), "msg_first");
        assert_eq!(m.conflict_count().unwrap(), 1);
        assert!(matches!(
            record(&mut m, &scope(), 12, "job_one", "msg_first"),
            Err(Error::Conflict)
        ));
        assert!(matches!(m.context(&scope(), 12), Err(Error::Unavailable)));
        assert_eq!(m.conflict_count().unwrap(), 2);
    }
    #[test]
    fn lookup_never_crosses_binding_or_treats_unknown_history_as_top_level() {
        let mut store = Store::memory(Limits::default()).unwrap();
        let mut m = Mappings::new(&mut store).unwrap();
        record(&mut m, &scope(), 11, "job_one", "msg_first").unwrap();
        for i in 0..6 {
            let mut changed = scope();
            match i {
                0 => changed.account_id = "other".into(),
                1 => changed.chat_id = -124,
                2 => changed.topic_id = Some(7),
                3 => changed.endpoint_id = "ag_other".into(),
                4 => changed.generation = 2,
                _ => changed.recipient_id = "ag_other".into(),
            }
            assert!(matches!(m.context(&changed, 11), Err(Error::Unavailable)));
        }
        assert!(matches!(m.context(&scope(), 99), Err(Error::Unavailable)));
    }
    #[test]
    fn normalization_has_one_no_topic_key_and_wire_zero_is_invalid() {
        assert_eq!(normalize_transport_topic(None).unwrap(), None);
        assert_eq!(normalize_transport_topic(Some(0)).unwrap(), None);
        assert_eq!(normalize_transport_topic(Some(7)).unwrap(), Some(7));
        assert!(normalize_transport_topic(Some(-1)).is_err());
        let mut store = Store::memory(Limits::default()).unwrap();
        let mut m = Mappings::new(&mut store).unwrap();
        let mut bad = scope();
        bad.topic_id = Some(0);
        assert!(matches!(
            record(&mut m, &bad, 11, "job_one", "msg_first"),
            Err(Error::Invalid)
        ));
        assert!(matches!(m.context(&bad, 11), Err(Error::Invalid)));
    }
    #[test]
    fn quota_refuses_without_evicting_existing_mapping_or_replay() {
        let mut store = Store::memory(Limits::default()).unwrap();
        let mut m = Mappings::new(&mut store).unwrap();
        record(&mut m, &scope(), 11, "job_one", "msg_first").unwrap();
        m.store.db.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<999)
            INSERT INTO telegram_reply_mappings SELECT 'synthetic',-123,0,'ag_relay',1,'ag_recipient',i+100,'msg_filler','filler_'||i,'synthetic',42 FROM n;").unwrap();
        assert!(matches!(
            record(&mut m, &scope(), 12, "job_two", "msg_second"),
            Err(Error::Full)
        ));
        assert_eq!(
            record(&mut m, &scope(), 11, "job_one", "msg_first").unwrap(),
            Recorded::Replay
        );
        assert_eq!(m.context(&scope(), 11).unwrap(), "msg_first");
        assert_eq!(m.conflict_count().unwrap(), 0);
    }
    #[test]
    fn global_quota_applies_even_to_a_fresh_binding() {
        let mut store = Store::memory(Limits::default()).unwrap();
        let mut m = Mappings::new(&mut store).unwrap();
        m.store.db.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10000)
            INSERT INTO telegram_reply_mappings SELECT 'synthetic',-123,0,'ag_'||(i/1000),1,'ag_recipient',i,'msg_filler','filler_'||i,'synthetic',42 FROM n;").unwrap();
        assert!(matches!(
            record(&mut m, &scope(), 11, "job_one", "msg_first"),
            Err(Error::Full)
        ));
    }
    #[cfg(unix)]
    #[test]
    fn reopen_preserves_immutable_receipt_and_conflict_counter() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("state");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        {
            let mut store = Store::open(&path, Limits::default()).unwrap();
            let mut m = Mappings::new(&mut store).unwrap();
            record(&mut m, &scope(), 11, "job_one", "msg_first").unwrap();
            assert!(matches!(
                record(&mut m, &scope(), 11, "job_one", "msg_other"),
                Err(Error::Conflict)
            ));
        }
        let mut store = Store::open(&path, Limits::default()).unwrap();
        let mut m = Mappings::new(&mut store).unwrap();
        assert_eq!(
            record(&mut m, &scope(), 11, "job_one", "msg_first").unwrap(),
            Recorded::Replay
        );
        assert_eq!(m.context(&scope(), 11).unwrap(), "msg_first");
        assert_eq!(m.conflict_count().unwrap(), 1);
        store.recheck_private_storage().unwrap();
    }
}
