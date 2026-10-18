//! Owner metadata exports and compare-before-clear. Never exports queue bodies.
use crate::{
    hub::{Directory, Error, Hub},
    recovery::{Audit, AUDIT_LIMIT},
    relay::Mailbox,
    store::{valid_id, Error as StorageError},
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Audit,
    Receipts,
}
#[derive(Serialize)]
pub struct Receipt {
    id: i64,
    endpoint_id: String,
    generation: i64,
    delivery_id: String,
    payload_bytes: i64,
    reason: crate::failures::FailureReason,
    at: i64,
}
#[derive(Serialize)]
pub struct Emergency {
    endpoint_id: String,
    actor_id: String,
    revision: i64,
    at: i64,
    count: i64,
}
#[derive(Serialize)]
#[serde(untagged)]
pub enum Records {
    Audit(Vec<Audit>),
    Receipts(Vec<Receipt>),
}
impl Records {
    fn len(&self) -> usize {
        match self {
            Self::Audit(v) => v.len(),
            Self::Receipts(v) => v.len(),
        }
    }
}
#[derive(Serialize)]
pub struct Export {
    pub version: u32,
    pub kind: Kind,
    pub endpoint_id: Option<String>,
    pub revision: Option<i64>,
    pub from: i64,
    pub through: i64,
    pub canonical_jsonl: String,
    pub checksum: String,
    pub records: Records,
    pub emergency_pauses: Vec<Emergency>,
}
fn collect(
    db: &Connection,
    kind: Kind,
    endpoint: Option<&str>,
    revision: Option<i64>,
    through: Option<i64>,
    hub: &str,
    workspace: &str,
) -> Result<Export, Error> {
    let maximum: i64 = match kind {
        Kind::Audit => {
            db.query_row("SELECT COALESCE(MAX(id),0) FROM (SELECT id FROM hub_owner_audit ORDER BY id LIMIT 1000)", [], |r| {
                r.get(0)
            })?
        }
        Kind::Receipts => db.query_row(
            "SELECT COALESCE(MAX(id),0) FROM (SELECT rowid AS id FROM failures WHERE endpoint=?1 ORDER BY rowid LIMIT 1000)",
            [endpoint],
            |r| r.get(0),
        )?,
    };
    let through = through.unwrap_or(maximum);
    if through < 0 {
        return Err(Error::Binding);
    }
    let records = match kind {
        Kind::Audit => {
            let mut q=db.prepare("SELECT id,actor,action,endpoint,transfer,revision,at FROM hub_owner_audit WHERE id<=?1 ORDER BY id LIMIT 1001")?;
            let rows = q
                .query_map([through], |r| {
                    Ok(Audit {
                        id: r.get(0)?,
                        actor_id: r.get(1)?,
                        action: r.get(2)?,
                        endpoint_id: r.get(3)?,
                        transfer_id: r.get(4)?,
                        previous_revision: r.get(5)?,
                        at: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Records::Audit(rows)
        }
        Kind::Receipts => {
            let mut q=db.prepare("SELECT rowid,endpoint,generation,delivery,payload_bytes,reason,created_at FROM failures WHERE endpoint=?1 AND rowid<=?2 ORDER BY rowid LIMIT 1001")?;
            let rows = q
                .query_map(params![endpoint, through], |r| {
                    Ok(Receipt {
                        id: r.get(0)?,
                        endpoint_id: r.get(1)?,
                        generation: r.get(2)?,
                        delivery_id: r.get(3)?,
                        payload_bytes: r.get(4)?,
                        reason: crate::failures::FailureReason::from_code(r.get(5)?)?,
                        at: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Records::Receipts(rows)
        }
    };
    if records.len() > 1000 {
        return Err(StorageError::Full.into());
    }
    let emergency_pauses = if matches!(kind, Kind::Audit) {
        let mut q=db.prepare("SELECT endpoint,actor,revision,at,count FROM hub_emergency_pauses ORDER BY endpoint LIMIT 21")?;
        let rows = q
            .query_map([], |r| {
                Ok(Emergency {
                    endpoint_id: r.get(0)?,
                    actor_id: r.get(1)?,
                    revision: r.get(2)?,
                    at: r.get(3)?,
                    count: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    } else {
        vec![]
    };
    if emergency_pauses.len() > 20 {
        return Err(StorageError::Full.into());
    }
    let from = match &records {
        Records::Audit(rows) => rows.first().map(|r| r.id).unwrap_or(0),
        Records::Receipts(rows) => rows.first().map(|r| r.id).unwrap_or(0),
    };
    // Fixed struct field order, serde_json escaping and UTF-8 LF-terminated lines.
    // The hash covers exactly the downloadable bytes, not rejected message bodies.
    #[derive(Serialize)]
    struct Header<'a> {
        version: u32,
        hub: &'a str,
        workspace: &'a str,
        kind: Kind,
        endpoint_id: Option<&'a str>,
        revision: Option<i64>,
        from: i64,
        through: i64,
    }
    fn line(out: &mut String, value: &impl Serialize) -> Result<(), Error> {
        out.push_str(&serde_json::to_string(value).map_err(|_| Error::Binding)?);
        out.push('\n');
        Ok(())
    }
    let mut canonical_jsonl = String::new();
    line(
        &mut canonical_jsonl,
        &Header {
            version: 1,
            hub,
            workspace,
            kind,
            endpoint_id: endpoint,
            revision,
            from,
            through,
        },
    )?;
    match &records {
        Records::Audit(rows) => {
            for row in rows {
                line(&mut canonical_jsonl, row)?;
            }
        }
        Records::Receipts(rows) => {
            for row in rows {
                line(&mut canonical_jsonl, row)?;
            }
        }
    }
    #[derive(Serialize)]
    struct EmergencyLine<'a> {
        emergency_pause: &'a Emergency,
    }
    for row in &emergency_pauses {
        line(
            &mut canonical_jsonl,
            &EmergencyLine {
                emergency_pause: row,
            },
        )?;
    }
    if canonical_jsonl.len() > 8 * 1024 * 1024 {
        return Err(StorageError::Full.into());
    }
    let checksum = format!("{:x}", Sha256::digest(canonical_jsonl.as_bytes()));
    Ok(Export {
        version: 1,
        kind,
        endpoint_id: endpoint.map(str::to_owned),
        revision,
        from,
        through,
        canonical_jsonl,
        checksum,
        records,
        emergency_pauses,
    })
}
impl<M: Mailbox + Directory> Hub<M> {
    pub fn metadata_archive(&self, kind: Kind, endpoint: Option<&str>) -> Result<Export, Error> {
        self.store.recheck_private_storage()?;
        self.recovery_schema()?;
        let revision = match (kind, endpoint) {
            (Kind::Audit, None) => None,
            (Kind::Receipts, Some(id)) => Some(self.view(id)?.revision),
            _ => return Err(Error::Binding),
        };
        collect(
            &self.store.db,
            kind,
            endpoint,
            revision,
            None,
            &self.own_id,
            &self.workspace,
        )
    }
    /// HTTP derives actor and checks confirmation, CSRF and current owner first.
    /// All bindings must be paused; no pending queue or rate state is cleared.
    pub fn clear_metadata_archive(
        &mut self,
        archive: &Clear,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        self.store.recheck_private_storage()?;
        if !valid_id(actor)
            || now < 0
            || archive.through <= 0
            || archive.checksum.len() != 64
            || !archive
                .checksum
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Binding);
        }
        self.recovery_schema()?;
        match (
            archive.kind,
            archive.endpoint_id.as_deref(),
            archive.revision,
        ) {
            // Evidence retention/hash-chain pruning is deliberately not implemented.
            (Kind::Audit, _, _) => return Err(Error::Binding),
            (Kind::Receipts, Some(id), Some(revision)) => {
                if self.view(id)?.revision != revision {
                    return Err(Error::Revision);
                }
            }
            _ => return Err(Error::Binding),
        }
        let tx = self.store.db.transaction()?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM endpoints WHERE paused=0)",
            [],
            |r| r.get(0),
        )?;
        if active {
            return Err(Error::Binding);
        }
        // Fixed table/column names only. Pausing does not cancel an already
        // leased operation; wait for its bounded completion before clearing.
        for (table, column, pending) in [
            ("hub_refetch_leases", "until_at", "1"),
            ("inbound", "lease_until", "pending=1"),
            ("telegram_chunks", "lease_until", "message_id IS NULL"),
            ("hub_failure_notices", "lease_until", "state=0"),
        ] {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                [table],
                |r| r.get(0),
            )?;
            if exists
                && tx.query_row(
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM {table} WHERE {column}>?1 AND ({pending}))"
                    ),
                    [now],
                    |r| r.get::<_, bool>(0),
                )?
            {
                return Err(Error::Binding);
            }
        }
        let current = collect(
            &tx,
            archive.kind,
            archive.endpoint_id.as_deref(),
            archive.revision,
            Some(archive.through),
            &self.own_id,
            &self.workspace,
        )?;
        if current.records.len() == 0 || current.checksum != archive.checksum {
            return Err(Error::Revision);
        }
        let audits: i64 = tx.query_row("SELECT COUNT(*) FROM hub_owner_audit", [], |r| r.get(0))?;
        if audits >= AUDIT_LIMIT {
            return Err(StorageError::Full.into());
        }
        // Only terminal failures: capacity receipts (including unrecovered Core
        // message handles), audit evidence and all queue/rate/circuit state stay.
        tx.execute(
            "DELETE FROM failures WHERE endpoint=?1 AND rowid<=?2",
            params![archive.endpoint_id, archive.through],
        )?;
        let full: bool = tx.query_row("SELECT (SELECT COUNT(*) FROM failures)>=10000 OR EXISTS(SELECT 1 FROM failures GROUP BY endpoint HAVING COUNT(*)>=1000)", [], |r|r.get(0))?;
        let was_blocked: bool = tx.query_row(
            "SELECT blocked FROM hub_identity WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let released = was_blocked && !full;
        if released {
            tx.execute("UPDATE hub_identity SET blocked=0 WHERE singleton=1", [])?;
        }
        tx.execute(
            "UPDATE hub_bindings SET revision=revision+1 WHERE endpoint=?1",
            [&archive.endpoint_id],
        )?;
        // This action's transfer field is fixed metadata, not a transfer ID.
        // Never erased by this API; saved is the owner's explicit declaration.
        let evidence = serde_json::to_string(&serde_json::json!({
            "from":current.from,"through":current.through,"rows":current.records.len(),
            "sha256":current.checksum,"archive_saved":true,"terminal_budget_released":released
        }))
        .map_err(|_| Error::Binding)?;
        tx.execute("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES(?1,'archive_receipts',?2,?3,?4,?5)",params![actor,archive.endpoint_id.as_deref().unwrap_or(""),evidence,archive.revision.unwrap_or(0),now])?;
        // Emergency safety rows remain in the DB, as does refetch rate history.
        tx.commit()?;
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clear {
    pub kind: Kind,
    pub endpoint_id: Option<String>,
    pub revision: Option<i64>,
    pub through: i64,
    pub checksum: String,
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        recovery::tests::fixture,
        store::{Limits, Store},
    };
    const NOW: i64 = 1_800_000_000;
    fn clear(a: &Export) -> Clear {
        Clear {
            kind: a.kind,
            endpoint_id: a.endpoint_id.clone(),
            revision: a.revision,
            through: a.through,
            checksum: a.checksum.clone(),
        }
    }
    #[test]
    fn audit_is_export_only_and_cannot_erase_owner_or_emergency_evidence() {
        let (mut hub, _) = fixture(Store::memory(Limits::default()).unwrap());
        hub.recovery_schema().unwrap();
        hub.store.db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) SELECT 'human_owner','fixture','ag_endpoint','',1,1 FROM n; INSERT INTO hub_emergency_pauses VALUES('ag_endpoint','human_owner',4,1,2);").unwrap();
        let export = hub.metadata_archive(Kind::Audit, None).unwrap();
        assert_eq!(export.records.len(), 1000);
        assert_eq!(export.emergency_pauses.len(), 1);
        assert_eq!(export.canonical_jsonl.lines().count(), 1002);
        assert_eq!(
            format!("{:x}", Sha256::digest(export.canonical_jsonl.as_bytes())),
            export.checksum
        );
        assert_eq!(
            hub.metadata_archive(Kind::Audit, None)
                .unwrap()
                .canonical_jsonl,
            export.canonical_jsonl
        );
        assert!(matches!(
            hub.clear_metadata_archive(&clear(&export), "human_owner", NOW),
            Err(Error::Binding)
        ));
        assert_eq!(hub.owner_audit_capacity().unwrap(), (true, 1));
    }
    #[test]
    fn receipt_clear_is_scope_revision_and_snapshot_fenced_and_does_not_clear_queues() {
        let (mut hub, _) = fixture(Store::memory(Limits::default()).unwrap());
        let before: i64 = hub
            .store
            .db
            .query_row(
                "SELECT COUNT(*) FROM fragments WHERE wire IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let export = hub
            .metadata_archive(Kind::Receipts, Some("ag_endpoint"))
            .unwrap();
        assert!(export.records.len() > 0);
        let mut request = clear(&export);
        request.revision = Some(999);
        assert!(matches!(
            hub.clear_metadata_archive(&request, "human_owner", NOW),
            Err(Error::Revision)
        ));
        let mut request = clear(&export);
        request.checksum = "0".repeat(64);
        assert!(matches!(
            hub.clear_metadata_archive(&request, "human_owner", NOW),
            Err(Error::Revision)
        ));
        hub.store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        assert!(hub
            .clear_metadata_archive(&clear(&export), "human_owner", NOW)
            .is_err());
        hub.store
            .db
            .execute("UPDATE endpoints SET paused=1", [])
            .unwrap();
        hub.hold_schema().unwrap();
        hub.store.db.execute_batch("CREATE TABLE hub_refetch_rate(audit INTEGER PRIMARY KEY,at INTEGER); INSERT INTO hub_refetch_rate VALUES(1,1800000000); INSERT INTO hub_capacity_receipts(endpoint,generation,delivery,message,reason,at) VALUES('ag_endpoint',1,'capacity_delivery','msg_retained','endpoint_hold_full',1); UPDATE hub_hold_health SET capacity_exceeded=1;").unwrap();
        hub.store
            .db
            .execute("UPDATE hub_identity SET blocked=1", [])
            .unwrap();
        hub.store.db.execute_batch("CREATE TABLE hub_refetch_leases(until_at INTEGER); INSERT INTO hub_refetch_leases VALUES(1800000001);").unwrap();
        assert!(matches!(
            hub.clear_metadata_archive(&clear(&export), "human_owner", NOW),
            Err(Error::Binding)
        ));
        hub.store
            .db
            .execute("UPDATE hub_refetch_leases SET until_at=?1", [NOW])
            .unwrap();
        hub.clear_metadata_archive(&clear(&export), "human_owner", NOW)
            .unwrap();
        assert_eq!(
            hub.store
                .db
                .query_row("SELECT COUNT(*) FROM failures", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            hub.store
                .db
                .query_row(
                    "SELECT COUNT(*) FROM fragments WHERE wire IS NOT NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            before
        );
        assert!(!hub.blocked().unwrap());
        assert_eq!(
            hub.store
                .db
                .query_row(
                    "SELECT message FROM hub_capacity_receipts WHERE recovered_at IS NULL",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "msg_retained"
        );
        assert!(hub
            .store
            .db
            .query_row("SELECT capacity_exceeded FROM hub_hold_health", [], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap());
        assert_eq!(
            hub.store
                .db
                .query_row("SELECT COUNT(*) FROM hub_refetch_rate", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let event = hub.owner_audit(None, 25).unwrap().remove(0);
        let evidence: serde_json::Value = serde_json::from_str(&event.transfer_id).unwrap();
        assert_eq!(evidence["archive_saved"], true);
        assert_eq!(evidence["terminal_budget_released"], true);
        assert_eq!(evidence["sha256"], export.checksum);
        assert!(hub.view("ag_endpoint").unwrap().paused);
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 5);
        assert_eq!(
            hub.owner_audit(None, 25).unwrap()[0].action,
            "archive_receipts"
        );
    }
    #[test]
    fn changed_receipts_or_emergency_snapshot_and_audit_failure_roll_back_clear() {
        let (mut hub, _) = fixture(Store::memory(Limits::default()).unwrap());
        let export = hub
            .metadata_archive(Kind::Receipts, Some("ag_endpoint"))
            .unwrap();
        hub.store
            .db
            .execute("UPDATE failures SET created_at=created_at+1", [])
            .unwrap();
        assert!(matches!(
            hub.clear_metadata_archive(&clear(&export), "human_owner", NOW),
            Err(Error::Revision)
        ));
        let export = hub
            .metadata_archive(Kind::Receipts, Some("ag_endpoint"))
            .unwrap();
        hub.store.db.execute_batch("CREATE TRIGGER deny_archive BEFORE INSERT ON hub_owner_audit BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        assert!(hub
            .clear_metadata_archive(&clear(&export), "human_owner", NOW)
            .is_err());
        assert_eq!(
            hub.metadata_archive(Kind::Receipts, Some("ag_endpoint"))
                .unwrap()
                .checksum,
            export.checksum
        );
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 4);
    }
}
