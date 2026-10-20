//! Agent binding management metadata. No tokens, message bodies or external IDs.
use crate::{
    hub::{Directory, Error, Hub},
    relay::Mailbox,
    scheduler::SharedHub,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
pub(crate) fn schema(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS hub_mask_key(singleton INTEGER PRIMARY KEY CHECK(singleton=1),key BLOB NOT NULL CHECK(length(key)=32));
    CREATE TABLE IF NOT EXISTS hub_create_requests(actor TEXT NOT NULL,key TEXT NOT NULL,fingerprint TEXT NOT NULL,endpoint TEXT NOT NULL,PRIMARY KEY(actor,key));
    CREATE TABLE IF NOT EXISTS hub_agent_managers(actor TEXT PRIMARY KEY);
    CREATE TABLE IF NOT EXISTS hub_binding_creators(endpoint TEXT PRIMARY KEY,actor TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS hub_retired_bindings(endpoint TEXT PRIMARY KEY,generation INTEGER NOT NULL,at INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS hub_management_notices(id INTEGER PRIMARY KEY,recipient TEXT NOT NULL,body TEXT NOT NULL,state INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,next_at INTEGER NOT NULL DEFAULT 0,lease_until INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS hub_management_requests(id INTEGER PRIMARY KEY AUTOINCREMENT,request_id TEXT NOT NULL UNIQUE,actor TEXT NOT NULL,status INTEGER NOT NULL,at INTEGER NOT NULL);")?;
    if db.query_row("SELECT COUNT(*) FROM hub_mask_key", [], |r| {
        r.get::<_, i64>(0)
    })? == 0
    {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|_| rusqlite::Error::InvalidQuery)?;
        db.execute("INSERT INTO hub_mask_key VALUES(1,?1)", [key.as_slice()])?;
    }
    Ok(())
}
#[derive(Default, serde::Serialize)]
pub struct NoticeStatus {
    pub pending: i64,
    pub accepted_by_core: i64,
    pub failed: i64,
}
pub(crate) fn notice_status(db: &Connection) -> rusqlite::Result<NoticeStatus> {
    db.query_row("SELECT COALESCE(SUM(state=0),0),COALESCE(SUM(state=1),0),COALESCE(SUM(state=2),0) FROM hub_management_notices",[],|r|Ok(NoticeStatus{pending:r.get(0)?,accepted_by_core:r.get(1)?,failed:r.get(2)?}))
}
pub(crate) fn agent(db: &Connection, id: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_agent_managers WHERE actor=?1)",
        [id],
        |r| r.get(0),
    )
}
pub(crate) fn register(db: &Connection, id: &str) -> rusqlite::Result<()> {
    if !agent(db, id)?
        && db.query_row("SELECT COUNT(*) FROM hub_agent_managers", [], |r| {
            r.get::<_, i64>(0)
        })? >= 1000
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    db.execute("INSERT OR IGNORE INTO hub_agent_managers VALUES(?1)", [id])?;
    Ok(())
}
/// Private per-hub keyed commitments prevent enumerating low-entropy targets.
pub(crate) fn key(db: &Connection) -> rusqlite::Result<[u8; 32]> {
    let bytes: Vec<u8> =
        db.query_row("SELECT key FROM hub_mask_key WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    bytes.try_into().map_err(|_| rusqlite::Error::InvalidQuery)
}
pub(crate) fn commitment(key: &[u8; 32], domain: &str, bytes: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    let mut hmac = Hmac::<Sha256>::new_from_slice(key).expect("fixed 32-byte HMAC key");
    hmac.update(domain.as_bytes());
    hmac.update(&[0]);
    hmac.update(bytes);
    format!("{:x}", hmac.finalize().into_bytes())
}
pub(crate) fn mask(value: &mut serde_json::Value, key: &[u8; 32]) {
    match value {
        serde_json::Value::Object(o) => {
            for (name, v) in o.iter_mut() {
                if matches!(
                    name.as_str(),
                    "chat_id"
                        | "topic_id"
                        | "allowed_senders"
                        | "address"
                        | "chat"
                        | "topic"
                        | "bot_id"
                        | "expected_bot_id"
                        | "verified_bot_id"
                        | "member_id"
                ) {
                    *v = serde_json::json!(format!(
                        "masked:{}",
                        &commitment(key, name, v.to_string().as_bytes())[..24]
                    ));
                } else {
                    mask(v, key);
                }
            }
        }
        serde_json::Value::Array(a) => {
            for v in a {
                mask(v, key);
            }
        }
        _ => {}
    }
}
pub(crate) fn masked_detail(db: &Connection, detail: &str) -> rusqlite::Result<String> {
    let mut v = serde_json::from_str(detail).unwrap_or(serde_json::Value::Null);
    mask(&mut v, &key(db)?);
    Ok(v.to_string())
}
/// Enqueued in the SAME transaction as the mutation and its authoritative audit.
pub(crate) fn changed(
    tx: &Transaction<'_>,
    actor: &str,
    action: &str,
    endpoint: &str,
    revision: i64,
    audit_id: i64,
) -> Result<(), Error> {
    if !agent(tx, actor)? {
        return Ok(());
    }
    let recipient: String = tx.query_row(
        "SELECT recipient FROM endpoints WHERE id=?1",
        [endpoint],
        |r| r.get(0),
    )?;
    let account: Option<String> = if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='telegram_targets')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        tx.query_row(
            "SELECT account FROM telegram_targets WHERE endpoint=?1",
            [endpoint],
            |r| r.get(0),
        )
        .optional()?
    } else {
        None
    };
    if action == "create_binding" {
        let count:i64=tx.query_row("SELECT COUNT(*) FROM hub_binding_creators c WHERE actor=?1 AND NOT EXISTS(SELECT 1 FROM hub_retired_bindings r WHERE r.endpoint=c.endpoint)",[actor],|r|r.get(0))?;
        if count >= 5 {
            return Err(Error::Binding);
        }
        tx.execute(
            "INSERT INTO hub_binding_creators VALUES(?1,?2)",
            params![endpoint, actor],
        )?;
    }
    let body=format!("Treer hub binding change (App-asserted management notice).\nCaller Agent: {actor}\nAction: {action}\nEndpoint: {endpoint}\nRevision before change: {revision}\nAccount: {}\nExternal target: redacted. Inspect the private hub settings to pause or review. This is not a delivery receipt.",account.as_deref().unwrap_or("not configured"));
    tx.execute(
        "INSERT INTO hub_management_notices(id,recipient,body) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET recipient=excluded.recipient,body=excluded.body,state=0,attempts=0,next_at=0,lease_until=0",
        params![audit_id, recipient, body],
    )?;
    Ok(())
}
#[cfg(test)]
pub(crate) fn record_request(
    db: &mut Connection,
    request: &str,
    actor: Option<&str>,
    status: u16,
    now: i64,
) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    tx.execute("INSERT INTO hub_management_requests(request_id,actor,status,at) VALUES(?1,?2,?3,?4) ON CONFLICT(request_id) DO UPDATE SET actor=excluded.actor,status=excluded.status,at=excluded.at",params![request,actor.unwrap_or("unverified"),status,now])?;
    tx.execute("DELETE FROM hub_management_requests WHERE id NOT IN (SELECT id FROM hub_management_requests ORDER BY id DESC LIMIT 1000)",[])?;
    tx.commit()
}
/// Notices bypass binding pause/deletion so containment is visible. They use
/// the hub's OWN workload identity and the transaction-pinned recipient only.
pub(crate) fn dispatch_one<M: Mailbox + Directory + Clone>(
    state: &SharedHub<M>,
    now: i64,
) -> Result<(), ()> {
    let (id, to, body, mailbox) = {
        let mut h = state.lock().map_err(|_| ())?;
        if h.store.recheck_private_storage().is_err() {
            return Err(());
        }
        let tx = h.store.db.transaction().map_err(|_| ())?;
        tx.execute("UPDATE hub_management_notices SET state=2 WHERE state=0 AND attempts>=8 AND lease_until<=?1",[now]).map_err(|_|())?;
        let row:Option<(i64,String,String)>=tx.query_row("SELECT id,recipient,body FROM hub_management_notices WHERE state=0 AND next_at<=?1 AND lease_until<=?1 AND attempts<8 ORDER BY id LIMIT 1",[now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_|())?;
        let Some((id, to, body)) = row else {
            tx.commit().map_err(|_| ())?;
            return Ok(());
        };
        tx.execute("UPDATE hub_management_notices SET attempts=attempts+1,lease_until=?2,next_at=?3 WHERE id=?1",params![id,now+60,now+60]).map_err(|_|())?;
        tx.commit().map_err(|_| ())?;
        (id, to, body, h.backend().clone())
    };
    let key = format!("hub-management-{id}-{:x}", Sha256::digest(body.as_bytes()));
    let result = mailbox.send(&to, &key, &body, &[], None);
    if result.is_ok() {
        state
            .lock()
            .map_err(|_| ())?
            .store
            .db
            .execute(
                "UPDATE hub_management_notices SET state=1 WHERE id=?1 AND body=?2",
                params![id, body],
            )
            .map_err(|_| ())?;
    }
    result.map(|_| ()).map_err(|_| ())
}

impl<M: Mailbox + Directory> Hub<M> {
    pub(crate) fn delete_binding_audited(
        &mut self,
        id: &str,
        revision: i64,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_not_terminal_held(id)?;
        let view = self.view(id)?;
        if !view.paused || view.revision != revision {
            return Err(Error::Revision);
        }
        self.store.recheck_private_storage()?;
        self.recovery_schema()?;
        let context = crate::owner_actions::AuditContext::new(actor, now)?;
        let tx = self.store.db.transaction()?;
        // Pending, parked, held, partial and still-leased work is never erased.
        for (table, condition) in [
            ("inbound", "(pending!=0 OR lease_until>?2)"),
            ("outbound", "pending!=0"),
            ("transfers", "state IN (0,1)"),
            ("hub_holds", "1"),
            ("hub_parked", "1"),
            ("telegram_chunks", "(lease_until>?2 OR message_id IS NULL)"),
            ("email_intents", "(state=0 OR lease_until>?2)"),
        ] {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                [table],
                |r| r.get(0),
            )?;
            if exists {
                let sql=format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE endpoint=?1 AND {condition} AND ?2>=0)");
                if tx.query_row(&sql, params![id, now], |r| r.get::<_, bool>(0))? {
                    return Err(Error::Revision);
                }
            }
        }
        crate::owner_actions::record(&tx, Some(&context), "delete_binding", id, revision)?;
        tx.execute(
            "INSERT INTO hub_retired_bindings VALUES(?1,?2,?3)",
            params![id, view.binding.generation, now],
        )?;
        tx.execute(
            "UPDATE hub_bindings SET revision=revision+1,reason='deleted' WHERE endpoint=?1",
            [id],
        )?;
        for table in ["telegram_targets", "email_targets"] {
            if tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                [table],
                |r| r.get::<_, bool>(0),
            )? {
                tx.execute(&format!("DELETE FROM {table} WHERE endpoint=?1"), [id])?;
            }
        }
        crate::telegram_reply_store::retire_deleted_endpoint(&tx, id)?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn per_hub_keys_persist_and_commitments_match_hmac_sha256() {
        assert_eq!(
            commitment(&[1; 32], "fixture", b"message"),
            "cd2994cd6e765138f9de385b312a610358542d2ebe2a1c22f94dcf7935fadda6"
        );
        let a = Connection::open_in_memory().unwrap();
        let b = Connection::open_in_memory().unwrap();
        schema(&a).unwrap();
        schema(&b).unwrap();
        let ka = key(&a).unwrap();
        schema(&a).unwrap();
        assert_eq!(ka, key(&a).unwrap());
        assert_ne!(
            commitment(&ka, "resume-preview-v1", b"same preview"),
            commitment(&key(&b).unwrap(), "resume-preview-v1", b"same preview")
        );
    }
    #[test]
    fn rejected_request_ring_is_bounded_and_contains_only_server_metadata() {
        let mut db = Connection::open_in_memory().unwrap();
        schema(&db).unwrap();
        for n in 0..1010 {
            record_request(&mut db, &format!("request-{n}"), None, 401, n).unwrap();
        }
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM hub_management_requests", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1000);
        record_request(&mut db, "request-1009", Some("ag_verified"), 403, 1020).unwrap();
        let row: (String, i64) = db
            .query_row(
                "SELECT actor,status FROM hub_management_requests WHERE request_id='request-1009'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("ag_verified".into(), 403));
    }
    #[test]
    fn identifiers_are_masked_recursively_but_core_ids_remain() {
        let mut v = serde_json::json!({"endpoint":"ag_endpoint","nested":{"chat_id":577127289,"address":"synthetic@example.test","allowed_senders":[577127289]}});
        mask(&mut v, &[1; 32]);
        let mut other = serde_json::json!({"nested":{"chat_id":577127289}});
        mask(&mut other, &[2; 32]);
        assert_ne!(v["nested"]["chat_id"], other["nested"]["chat_id"]);
        assert_eq!(v["endpoint"], "ag_endpoint");
        assert!(!v.to_string().contains("577127289"));
        assert!(!v.to_string().contains("synthetic@example.test"));
    }
    #[test]
    fn exhausted_notice_is_visible_failed_without_another_send() {
        let (hub, _) = crate::recovery::tests::fixture(
            crate::store::Store::memory(Default::default()).unwrap(),
        );
        hub.store.db.execute("INSERT INTO hub_management_notices(id,recipient,body,attempts,lease_until) VALUES(1,'ag_recipient','synthetic',8,100)",[]).unwrap();
        let state = std::sync::Arc::new(std::sync::Mutex::new(hub));
        // The fixture panics if a network send is attempted.
        dispatch_one(&state, 101).unwrap();
        let s = notice_status(&state.lock().unwrap().store.db).unwrap();
        assert_eq!((s.pending, s.accepted_by_core, s.failed), (0, 0, 1));
    }
}
